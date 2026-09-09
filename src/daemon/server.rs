//! The socket server: one tokio task per connection, NDJSON JSON-RPC in and
//! out (P2-U7).
//!
//! Phase 2 serves `initialize`, `daemon.status` and `daemon.stop`, and answers
//! everything else with `-32601` once the connection has handshaken and with
//! `-32000` before it has. The two `daemon.*` methods are lifecycle surface
//! rather than domain surface, so they are reachable **before** `initialize`:
//! `mp daemon status` must be able to describe a daemon whose protocol range
//! it cannot even negotiate, and `mp daemon stop` must be able to end one. The
//! handshake itself lives in [`super::session`], which owns the per-connection
//! state [`dispatch_request`] gates on.
//!
//! Shutdown is a [`watch`] channel rather than a flag: the accept loop selects
//! on it, a `daemon.stop` handler sends on it after its response is flushed,
//! and the signal task sends on it from outside any connection.

use std::sync::Arc;

use anyhow::{Context, Result};
use log::{debug, info, warn};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::watch;

use mp_protocol::frame::{self, Decoder, FrameError};
use mp_protocol::{
    ErrorResponse, Request, RequestId, Response, RpcError, JSONRPC_VERSION, MAX_REQUEST_BYTES,
};

use super::runtime::InstanceMeta;
use super::session::{ConfigReport, Session};

/// JSON-RPC standard codes this unit emits. The daemon range lives in
/// [`mp_protocol::ErrorCode`] and is not reachable until P2-U9.
const PARSE_ERROR: i32 = -32700;
const INVALID_REQUEST: i32 = -32600;
const METHOD_NOT_FOUND: i32 = -32601;

/// How much one read may pull off a connection at a time. The frame cap is
/// enforced by the [`Decoder`], not by this buffer.
const READ_CHUNK: usize = 8 * 1024;

/// One account as `daemon.status` reports it.
///
/// Phase 2 never opens a store, so the only state a real runtime could be in is
/// `opening`; `ready` and `blocked` arrive with the account runtimes in Phase 5.
#[derive(Clone, Debug)]
pub struct AccountStatus {
    /// The configured account name.
    pub name: String,
    /// One of `opening`, `ready`, `blocked`.
    pub state: String,
}

/// Everything a served method may read. Immutable for the daemon's lifetime in
/// Phase 2, so it needs no lock; account runtimes will make it interior-mutable.
#[derive(Debug)]
pub struct DaemonState {
    /// What this daemon published in `daemon.json`.
    pub meta: InstanceMeta,
    /// Accounts the daemon knows about, empty unless account runtimes were
    /// opted into with `MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES=1`.
    pub accounts: Vec<AccountStatus>,
    /// How the configuration looked when this daemon started, reported by the
    /// handshake as `config_status`.
    pub config: ConfigReport,
}

impl DaemonState {
    /// The `result` of `daemon.status`.
    ///
    /// The CLI adds `"running": true` and prints the rest verbatim, so this
    /// object is the contract in `tests/daemon_lifecycle.rs` minus that key.
    pub fn status_result(&self) -> Value {
        json!({
            "instance_id": self.meta.instance_id,
            "app_version": self.meta.app_version,
            "protocol": {"min": self.meta.protocol_min, "max": self.meta.protocol_max},
            "pid": self.meta.pid,
            "started_at": self.meta.started_at,
            "data_dir": self.meta.data_dir.display().to_string(),
            "config_dir": self.meta.config_dir.display().to_string(),
            "accounts": self
                .accounts
                .iter()
                .map(|a| json!({"name": a.name, "state": a.state}))
                .collect::<Vec<_>>(),
        })
    }
}

/// Accept connections until `shutdown` fires, then return.
///
/// Open connections are not drained: the caller unlinks the runtime files and
/// exits immediately afterwards, and a client that loses its socket mid-call
/// reconnects. Draining belongs with the operations that will need settling
/// (the undo-send hold, the pending-op drainer), which Phase 5 introduces.
pub async fn serve(
    listener: UnixListener,
    state: Arc<DaemonState>,
    shutdown: watch::Sender<bool>,
) -> Result<()> {
    let mut stop = shutdown.subscribe();
    info!(
        "[daemon] serving instance {} on {}",
        state.meta.instance_id,
        listener
            .local_addr()
            .ok()
            .and_then(|a| a.as_pathname().map(|p| p.display().to_string()))
            .unwrap_or_else(|| "<unnamed>".to_string())
    );

    loop {
        tokio::select! {
            changed = stop.changed() => {
                // A closed channel means every sender is gone, which cannot
                // happen while the caller holds one; either way, stop serving.
                if changed.is_err() || *stop.borrow() {
                    break;
                }
            }
            accepted = listener.accept() => match accepted {
                Ok((stream, _addr)) => {
                    let state = Arc::clone(&state);
                    let shutdown = shutdown.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, state, shutdown).await {
                            debug!("[daemon] connection ended: {e:#}");
                        }
                    });
                }
                Err(e) => {
                    // A failed accept is per-connection (EMFILE, ECONNABORTED)
                    // and never a reason to take the daemon down.
                    warn!("[daemon] accept failed: {e}");
                }
            },
        }
    }

    info!("[daemon] stopped accepting connections");
    Ok(())
}

/// Read frames off one connection until it closes or breaks the framing rules.
///
/// A framing error closes **this** connection and nothing else: the daemon has
/// no way to resynchronise a stream whose boundaries it has lost, but every
/// other client is unaffected.
async fn handle_connection(
    stream: tokio::net::UnixStream,
    state: Arc<DaemonState>,
    shutdown: watch::Sender<bool>,
) -> Result<()> {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut decoder = Decoder::new(MAX_REQUEST_BYTES);
    let mut buf = vec![0u8; READ_CHUNK];
    // One handshake per connection, so the session dies with the connection and
    // nothing has to expire it.
    let mut session = Session::new();

    loop {
        let read = reader.read(&mut buf).await.context("reading a frame")?;
        if read == 0 {
            return Ok(());
        }
        let frames = match decoder.push(&buf[..read]) {
            Ok(frames) => frames,
            Err(e) => {
                let response = ErrorResponse {
                    jsonrpc: JSONRPC_VERSION.to_string(),
                    id: None,
                    error: frame_error(&e),
                };
                let bytes = frame::encode(&response)?;
                let _ = writer.write_all(&bytes).await;
                let _ = writer.flush().await;
                return Ok(());
            }
        };

        for value in frames {
            let (reply, stop) = dispatch_request(value, &state, &mut session);
            if let Some(reply) = reply {
                writer
                    .write_all(&frame::encode(&reply)?)
                    .await
                    .context("writing a response")?;
                writer.flush().await.context("flushing a response")?;
            }
            if stop {
                // Only now, with the response on the wire, does the client
                // learn the daemon is going away.
                info!("[daemon] shutdown requested over the socket");
                let _ = shutdown.send(true);
                return Ok(());
            }
        }
    }
}

/// Answer one frame.
///
/// Returns the message to write back (`None` for a notification, which by
/// JSON-RPC gets no answer) and whether the daemon should shut down afterwards.
fn dispatch_request(
    value: Value,
    state: &DaemonState,
    session: &mut Session,
) -> (Option<Value>, bool) {
    let request: Request = match serde_json::from_value(value) {
        Ok(request) => request,
        Err(e) => {
            return (
                Some(error_value(
                    None,
                    INVALID_REQUEST,
                    format!("not a JSON-RPC request: {e}"),
                    None,
                )),
                false,
            )
        }
    };

    if request.jsonrpc != JSONRPC_VERSION {
        return (
            Some(error_value(
                request.id.clone(),
                INVALID_REQUEST,
                format!("jsonrpc must be \"{JSONRPC_VERSION}\""),
                None,
            )),
            false,
        );
    }

    // The handshake gate: every method except `initialize` and the two
    // lifecycle methods is `-32000 not_initialized` until this connection has
    // handshaken. The refusal is per request, so the connection stays usable
    // and the `initialize` that should have come first still works on it.
    if let Some(refusal) = session.gate(&request.method) {
        return (
            Some(error_value(
                request.id,
                refusal.code,
                refusal.message,
                refusal.data,
            )),
            false,
        );
    }

    match request.method.as_str() {
        "initialize" => match session.initialize(&request.params, state) {
            Ok(result) => (result_value(request.id, result), false),
            Err(refusal) => (
                Some(error_value(
                    request.id,
                    refusal.code,
                    refusal.message,
                    refusal.data,
                )),
                false,
            ),
        },
        "daemon.status" => (result_value(request.id, state.status_result()), false),
        "daemon.stop" => (result_value(request.id, json!({"stopping": true})), true),
        other => (
            Some(error_value(
                request.id,
                METHOD_NOT_FOUND,
                format!("unknown method {other}"),
                None,
            )),
            false,
        ),
    }
}

/// A `Response`, or nothing when the request carried no id (a notification).
fn result_value(id: Option<RequestId>, result: Value) -> Option<Value> {
    let id = id?;
    let response = Response {
        jsonrpc: JSONRPC_VERSION.to_string(),
        id,
        result,
    };
    serde_json::to_value(response).ok()
}

/// An `ErrorResponse` as a `Value`, so both arms of a dispatch share one type.
fn error_value(id: Option<RequestId>, code: i32, message: String, data: Option<Value>) -> Value {
    let response = ErrorResponse {
        jsonrpc: JSONRPC_VERSION.to_string(),
        id,
        error: RpcError {
            code,
            message,
            data,
        },
    };
    serde_json::to_value(response).unwrap_or_else(|_| {
        json!({"jsonrpc": JSONRPC_VERSION, "error": {"code": code, "message": "internal error"}})
    })
}

/// Map a framing failure onto the wire error that describes it.
fn frame_error(error: &FrameError) -> RpcError {
    match error {
        FrameError::TooLarge { limit, seen } => RpcError {
            code: mp_protocol::ErrorCode::FrameTooLarge.code(),
            message: error.to_string(),
            data: Some(json!({"limit": limit, "seen": seen})),
        },
        FrameError::InvalidUtf8 | FrameError::InvalidJson(_) => RpcError {
            code: PARSE_ERROR,
            message: error.to_string(),
            data: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A daemon state with no accounts, which is the Phase 2 shape.
    fn state_fixture() -> DaemonState {
        use std::path::PathBuf;
        DaemonState {
            meta: InstanceMeta {
                app_version: "0.0.0-test".to_string(),
                protocol_min: mp_protocol::PROTOCOL_MIN,
                protocol_max: mp_protocol::PROTOCOL_MAX,
                instance_id: "abcd".to_string(),
                pid: 42,
                started_at: "2026-01-01T00:00:00Z".to_string(),
                data_dir: PathBuf::from("/tmp/data"),
                config_dir: PathBuf::from("/tmp/config"),
            },
            accounts: Vec::new(),
            config: ConfigReport::Absent {
                path: PathBuf::from("/tmp/config/config.toml"),
            },
        }
    }

    fn request(method: &str, id: Option<i64>) -> Value {
        let mut value = json!({"jsonrpc": "2.0", "method": method, "params": {}});
        if let Some(id) = id {
            value["id"] = json!(id);
        }
        value
    }

    /// `daemon.status` answers before any handshake, which is what lets
    /// `mp daemon status` describe an incompatible daemon.
    #[test]
    fn status_answers_without_an_initialize() {
        let state = state_fixture();
        let mut session = Session::new();
        let (reply, stop) =
            dispatch_request(request("daemon.status", Some(7)), &state, &mut session);
        let reply = reply.expect("a request with an id is answered");
        assert!(!stop);
        assert_eq!(reply["id"], json!(7));
        assert_eq!(reply["result"]["instance_id"], json!("abcd"));
        assert_eq!(reply["result"]["accounts"], json!([]));
    }

    /// `daemon.stop` is answered first and only then ends the daemon.
    #[test]
    fn stop_replies_before_it_shuts_down() {
        let state = state_fixture();
        let mut session = Session::new();
        let (reply, stop) = dispatch_request(request("daemon.stop", Some(1)), &state, &mut session);
        assert!(stop, "daemon.stop asks for a shutdown");
        assert_eq!(reply.expect("answered")["result"]["stopping"], json!(true));
    }

    /// A domain method before the handshake is `-32000`, not `-32601`: the gate
    /// runs before the method lookup, so an uninitialized client cannot probe
    /// which methods a daemon serves.
    #[test]
    fn a_domain_method_before_initialize_is_not_initialized() {
        let state = state_fixture();
        let mut session = Session::new();
        let (reply, stop) =
            dispatch_request(request("account.list", Some(2)), &state, &mut session);
        assert!(!stop);
        assert_eq!(reply.expect("answered")["error"]["code"], json!(-32000));
    }

    /// After the handshake, an unknown method is `-32601` again, until the
    /// domain families land.
    #[test]
    fn an_unknown_method_after_initialize_is_method_not_found() {
        let state = state_fixture();
        let mut session = Session::new();
        let initialize = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "client": {"type": "cli", "version": "0.9.0"},
                "protocol": {"min": 1, "max": 1},
                "capabilities": {"required": [], "optional": []},
                "identity": {"data_dir": "/tmp/data", "config_dir": "/tmp/config"},
            },
        });
        let (reply, _) = dispatch_request(initialize, &state, &mut session);
        assert!(
            reply.expect("answered")["result"]["instance_id"] == json!("abcd"),
            "the handshake succeeds against the fixture's own directories"
        );

        let (reply, stop) =
            dispatch_request(request("account.list", Some(2)), &state, &mut session);
        assert!(!stop);
        assert_eq!(reply.expect("answered")["error"]["code"], json!(-32601));
    }

    /// A request without an id is a notification: no answer, no shutdown.
    #[test]
    fn a_notification_gets_no_response() {
        let state = state_fixture();
        let mut session = Session::new();
        let (reply, stop) = dispatch_request(request("daemon.status", None), &state, &mut session);
        assert!(reply.is_none());
        assert!(!stop);
    }
}
