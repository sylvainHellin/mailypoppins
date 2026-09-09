//! One connection: connect, `initialize`, `call`.
//!
//! Calls are sequential by construction, because [`Connection::call`] takes
//! `&mut self`: one request goes out, its answer comes back, and the id counter
//! never has more than one outstanding value. That is what lets the reader
//! treat a reply carrying another id as a protocol violation rather than match
//! answers against a table of pending calls. Server-initiated notifications
//! (`state.event` and friends) are skipped; delivering them is Phase 3 work and
//! needs an owned reader task, not a borrowed one.

use std::collections::VecDeque;
use std::path::Path;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use mp_protocol::frame::{self, Decoder};
use mp_protocol::{Request, RequestId, RpcError, JSONRPC_VERSION, PROTOCOL_MAX, PROTOCOL_MIN};

use crate::types::{
    ClientError, ClientInfo, ConfigStatus, Identity, InitializeResult, PlatformInfo,
};
use crate::MAX_RESPONSE_BYTES;

/// How much one read pulls off the socket. The frame cap is the decoder's
/// business, not this buffer's.
const READ_CHUNK: usize = 8 * 1024;

/// A live connection to a daemon.
#[derive(Debug)]
pub struct Connection {
    stream: UnixStream,
    decoder: Decoder,
    /// Frames decoded but not yet consumed, in arrival order.
    pending: VecDeque<Value>,
    /// The id of the next request; monotonic for the connection's lifetime.
    next_id: i64,
}

impl Connection {
    /// Connect to a daemon socket. A path with nothing listening is
    /// [`ClientError::NotRunning`] rather than an I/O fault: it is the one
    /// failure a caller answers by starting a daemon.
    pub async fn connect(socket: &Path) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(socket).await.map_err(|e| {
            use std::io::ErrorKind::{ConnectionRefused, NotFound};
            match e.kind() {
                NotFound | ConnectionRefused => ClientError::NotRunning,
                _ => ClientError::Io(e),
            }
        })?;
        Ok(Connection {
            stream,
            decoder: Decoder::new(MAX_RESPONSE_BYTES),
            pending: VecDeque::new(),
            next_id: 1,
        })
    }

    /// Perform the handshake. `required` capabilities the daemon does not offer are refused with
    /// `capability_missing`; `optional` ones declare interest and never refuse a
    /// connection. A second call on one connection is refused by the daemon with
    /// `-32600`, and is sent rather than short-circuited here, so the client
    /// cannot disagree with the daemon about which connections are initialized.
    pub async fn initialize(
        &mut self,
        info: ClientInfo,
        id: Identity,
        required: &[&str],
        optional: &[&str],
    ) -> Result<InitializeResult, ClientError> {
        let params = json!({
            "client": {"type": info.kind.as_str(), "version": info.app_version},
            "protocol": {"min": PROTOCOL_MIN, "max": PROTOCOL_MAX},
            "capabilities": {"required": required, "optional": optional},
            "identity": {
                "data_dir": id.data_dir.display().to_string(),
                "config_dir": id.config_dir.display().to_string(),
            },
        });
        let result = self.call("initialize", params).await?;
        parse_initialize(&result)
    }

    /// Call one method and return its `result`.
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, ClientError> {
        let id = self.next_id;
        self.next_id += 1;

        let request = Request {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: Some(RequestId::Num(id)),
            method: method.to_string(),
            params,
        };
        let bytes = frame::encode(&request)
            .map_err(|e| ClientError::Protocol(format!("encoding {method}: {e}")))?;
        self.stream.write_all(&bytes).await?;
        self.stream.flush().await?;

        let reply = self.read_reply(id).await?;
        if let Some(error) = reply.get("error") {
            let error: RpcError = serde_json::from_value(error.clone()).map_err(|e| {
                ClientError::Protocol(format!("the {method} error is not a JSON-RPC error: {e}"))
            })?;
            return Err(ClientError::Rpc(error));
        }
        reply.get("result").cloned().ok_or_else(|| {
            ClientError::Protocol(format!("the answer to {method} carried no result"))
        })
    }

    /// Read frames until the answer to `id` arrives.
    async fn read_reply(&mut self, id: i64) -> Result<Value, ClientError> {
        loop {
            while let Some(value) = self.pending.pop_front() {
                match classify(&value, id) {
                    Frame::Ours => return Ok(value),
                    Frame::Ignorable => continue,
                    Frame::Foreign(other) => {
                        return Err(ClientError::Protocol(format!(
                            "the daemon answered id {other} while {id} was outstanding"
                        )))
                    }
                }
            }

            let mut buf = [0u8; READ_CHUNK];
            let read = self.stream.read(&mut buf).await?;
            if read == 0 {
                return Err(ClientError::Protocol(
                    "the daemon closed the connection without answering".to_string(),
                ));
            }
            let frames = self
                .decoder
                .push(&buf[..read])
                .map_err(|e| ClientError::Protocol(e.to_string()))?;
            self.pending.extend(frames);
        }
    }
}

/// What one decoded frame is, relative to the call waiting for an answer.
enum Frame {
    /// The answer to the outstanding call.
    Ours,
    /// A notification, which Phase 2 has no reader task to deliver.
    Ignorable,
    /// An answer to an id nobody asked for; carries it, for the message.
    Foreign(String),
}

/// Classify one frame against the outstanding request id.
fn classify(value: &Value, id: i64) -> Frame {
    match value.get("id") {
        Some(Value::Number(n)) if n.as_i64() == Some(id) => Frame::Ours,
        // An unattributable parse error carries no id and is about our frame.
        None | Some(Value::Null) if value.get("error").is_some() => Frame::Ours,
        None | Some(Value::Null) => Frame::Ignorable,
        Some(other) => Frame::Foreign(other.to_string()),
    }
}

/// Turn the daemon's `initialize` result into the typed one.
fn parse_initialize(result: &Value) -> Result<InitializeResult, ClientError> {
    let app_version = string_at(result, &["daemon", "version"])?;
    let protocol = result
        .pointer("/protocol/selected")
        .and_then(Value::as_u64)
        .ok_or_else(|| missing("protocol.selected"))? as u32;
    let instance_id = string_at(result, &["instance_id"])?;

    let capabilities = result
        .get("capabilities")
        .and_then(Value::as_array)
        .ok_or_else(|| missing("capabilities"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| missing("a capability identifier as a string"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let platform = PlatformInfo {
        os: string_at(result, &["platform", "os"])?,
        transport: string_at(result, &["platform", "transport"])?,
    };

    let status = result
        .get("config_status")
        .ok_or_else(|| missing("config_status"))?;
    let config = match string_at(status, &["state"])?.as_str() {
        "absent" => ConfigStatus::Absent,
        "ok" => ConfigStatus::Loaded {
            accounts: status
                .get("accounts")
                .and_then(Value::as_u64)
                .ok_or_else(|| missing("config_status.accounts"))? as usize,
        },
        "invalid" => ConfigStatus::Invalid {
            message: first_problem(status),
        },
        other => {
            return Err(ClientError::Protocol(format!(
                "unknown config_status.state {other:?}"
            )))
        }
    };

    Ok(InitializeResult {
        app_version,
        protocol,
        instance_id,
        capabilities,
        platform,
        config,
        lifecycle: result.get("lifecycle").cloned().unwrap_or(Value::Null),
    })
}

/// The first entry of `config_status.problems`, as text. A problem is a string
/// today; an object carrying a `message` is accepted so a later, richer
/// diagnostic does not break an older client.
fn first_problem(status: &Value) -> String {
    let first = status
        .get("problems")
        .and_then(Value::as_array)
        .and_then(|problems| problems.first());
    match first {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Object(map)) => map
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("the configuration on disk is invalid")
            .to_string(),
        _ => "the configuration on disk is invalid".to_string(),
    }
}

/// A string at a path of object keys, or the protocol error naming it.
fn string_at(value: &Value, path: &[&str]) -> Result<String, ClientError> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(key).ok_or_else(|| missing(&path.join(".")))?;
    }
    cursor
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| missing(&path.join(".")))
}

/// The protocol error for a field the daemon owed us.
fn missing(what: &str) -> ClientError {
    ClientError::Protocol(format!("the initialize result has no {what}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result_fixture() -> Value {
        json!({
            "daemon": {"version": "0.9.0"},
            "protocol": {"selected": 1},
            "instance_id": "abcd",
            "capabilities": ["daemon.status"],
            "platform": {"os": "linux", "transport": "unix_socket"},
            "lifecycle": {"restart_required": false},
            "config_status": {"state": "ok", "path": "/c/config.toml", "accounts": 2, "problems": []},
        })
    }

    #[test]
    fn a_complete_result_parses_field_by_field() {
        let parsed = parse_initialize(&result_fixture()).expect("parses");
        assert_eq!(parsed.app_version, "0.9.0");
        assert_eq!(parsed.protocol, 1);
        assert_eq!(parsed.instance_id, "abcd");
        assert_eq!(parsed.capabilities, vec!["daemon.status".to_string()]);
        assert_eq!(parsed.platform.transport, "unix_socket");
        assert_eq!(parsed.config, ConfigStatus::Loaded { accounts: 2 });
    }

    #[test]
    fn the_three_config_states_map_onto_the_three_variants() {
        let mut absent = result_fixture();
        absent["config_status"] = json!({"state": "absent", "accounts": 0, "problems": []});
        assert_eq!(
            parse_initialize(&absent).expect("parses").config,
            ConfigStatus::Absent
        );

        let mut invalid = result_fixture();
        invalid["config_status"] =
            json!({"state": "invalid", "accounts": 0, "problems": ["line 4: bad backend"]});
        assert_eq!(
            parse_initialize(&invalid).expect("parses").config,
            ConfigStatus::Invalid {
                message: "line 4: bad backend".to_string()
            }
        );
    }

    #[test]
    fn a_result_missing_a_field_is_a_protocol_error_naming_it() {
        let mut broken = result_fixture();
        broken["daemon"] = json!({});
        let error = parse_initialize(&broken).expect_err("refused");
        assert!(
            matches!(&error, ClientError::Protocol(message) if message.contains("daemon.version")),
            "got {error:?}"
        );
    }
}
