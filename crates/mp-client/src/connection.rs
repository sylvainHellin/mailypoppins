//! One connection: connect, `initialize`, `call`.
//!
//! Calls are sequential by construction, because [`Connection::call`] takes
//! `&mut self`: one request goes out, its answer comes back, and the id counter
//! never has more than one outstanding value. That is what lets the reader
//! treat a reply carrying another id as a protocol violation rather than match
//! answers against a table of pending calls.
//!
//! Server-initiated notifications (`state.event` and friends) arrive on the
//! same socket, at any moment, including in the middle of waiting for a reply.
//! [`Connection::call`] therefore buffers them instead of dropping them, and
//! [`Connection::next_notification`] hands them over in arrival order. A method
//! rather than a subscription receiver, so the borrowed-reader design stays
//! intact: an owned reader task would have to undo it.
//!
//! # A timed-out connection is closed
//!
//! A call abandoned before its answer arrived leaves the connection in a state
//! no later call can recover from: the answer is still owed, and it may land
//! in the middle of the next call's wait, or the request itself may have been
//! cut off half written. So the first call abandoned that way closes the
//! connection, and every call after it answers [`ClientError::Closed`] without
//! touching the socket. A long-lived client that sees a
//! [`ClientError::Timeout`] or a [`ClientError::Closed`] opens a new
//! connection; it never retries on the old one.

use std::collections::VecDeque;
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use mp_protocol::frame::{self, Decoder};
use mp_protocol::{
    Notification, Request, RequestId, RpcError, JSONRPC_VERSION, PROTOCOL_MAX, PROTOCOL_MIN,
};

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
    /// Notifications that arrived while a call was outstanding, in arrival
    /// order, waiting for [`Connection::next_notification`].
    notifications: VecDeque<Value>,
    /// The id of the next request; monotonic for the connection's lifetime.
    next_id: i64,
    /// A call is between writing its request and reading its answer. Still
    /// set when the next call starts means the last one was abandoned (its
    /// future dropped by a timeout) and the connection is out of step.
    in_flight: bool,
    /// The connection was abandoned by a timed-out call and answers
    /// [`ClientError::Closed`] from now on.
    closed: bool,
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
            notifications: VecDeque::new(),
            next_id: 1,
            in_flight: false,
            closed: false,
        })
    }

    /// Whether a timed-out call closed this connection, after which every call
    /// answers [`ClientError::Closed`] and the caller reopens.
    pub fn is_closed(&self) -> bool {
        self.closed || self.in_flight
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
    ///
    /// [`ClientError::Closed`] without a write when an earlier call on this
    /// connection timed out or was otherwise abandoned mid-call (see the module
    /// docs): its answer is still owed, so this one could not be told apart
    /// from it.
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, ClientError> {
        if self.in_flight {
            // The last call's future was dropped between its write and its
            // answer, by a timeout the caller wrapped around it. Close rather
            // than read what may be that call's reply as this one's.
            self.closed = true;
        }
        if self.closed {
            return Err(ClientError::Closed);
        }
        self.in_flight = true;
        let answer = self.call_unguarded(method, params).await;
        self.in_flight = false;
        answer
    }

    /// [`Connection::call`] without the abandoned-call guard.
    async fn call_unguarded(&mut self, method: &str, params: Value) -> Result<Value, ClientError> {
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

    /// [`Connection::call`] under a budget of the caller's choosing:
    /// [`ClientError::Timeout`] when the answer has not arrived within
    /// `budget`.
    ///
    /// [`Connection::call`] itself waits as long as the daemon takes, which is
    /// right for nothing a user is waiting on, so every interactive caller
    /// wants this one; the budget is the caller's because it depends on the
    /// method (a store read answers in milliseconds, a mutation spans a round
    /// trip to the mail server).
    ///
    /// **A timeout closes the connection.** The answer is still owed, and the
    /// budget may have run out in the middle of writing the request, so the
    /// daemon may be holding half a frame: neither is something a later call
    /// could recover from. The connection is therefore marked closed and its
    /// write half shut down, which the daemon reads as a disconnect (a partial
    /// frame is discarded with the connection, never executed). Every later
    /// call answers [`ClientError::Closed`] and [`Connection::next_notification`]
    /// answers `None`; a caller that wants to keep talking to the daemon opens
    /// a new connection.
    pub async fn call_within(
        &mut self,
        method: &str,
        params: Value,
        budget: Duration,
    ) -> Result<Value, ClientError> {
        match tokio::time::timeout(budget, self.call(method, params)).await {
            Ok(answer) => answer,
            Err(_) => {
                self.close().await;
                Err(ClientError::Timeout {
                    method: method.to_string(),
                    after: budget,
                })
            }
        }
    }

    /// Mark the connection closed and shut its write half, so the daemon sees
    /// the disconnect now rather than when the value is dropped. Best effort:
    /// a socket that is already gone is the state being asked for.
    async fn close(&mut self) {
        self.closed = true;
        self.pending.clear();
        let _ = self.stream.shutdown().await;
    }

    /// The next server-initiated notification, or `None` when the daemon closed
    /// the connection.
    ///
    /// Returns a notification an earlier [`Connection::call`] already buffered
    /// if there is one, and otherwise reads until one arrives. A reply frame
    /// seen here answers no outstanding call, so it is kept for the next call
    /// to refuse by id rather than swallowed.
    pub async fn next_notification(&mut self) -> Option<Notification> {
        loop {
            // A closed connection is one the caller has to reopen; reading on
            // would hand out events past a call whose answer never came.
            if self.is_closed() {
                return None;
            }
            if let Some(value) = self.notifications.pop_front() {
                match serde_json::from_value(value) {
                    Ok(notification) => return Some(notification),
                    // A frame shaped like a notification that does not parse as
                    // one is not worth killing the connection over, and the
                    // caller is waiting for the next real event.
                    Err(_) => continue,
                }
            }
            // Sort what is already decoded, keeping the arrival order of both
            // classes: a reply here answers no outstanding call and is left for
            // the next `call` to refuse by id.
            let mut replies = VecDeque::new();
            while let Some(value) = self.pending.pop_front() {
                if is_notification(&value) {
                    self.notifications.push_back(value);
                } else {
                    replies.push_back(value);
                }
            }
            self.pending = replies;
            if !self.notifications.is_empty() {
                continue;
            }

            let mut buf = [0u8; READ_CHUNK];
            match self.stream.read(&mut buf).await {
                Ok(0) | Err(_) => return None,
                Ok(read) => match self.decoder.push(&buf[..read]) {
                    Ok(frames) => self.pending.extend(frames),
                    Err(_) => return None,
                },
            }
        }
    }

    /// Read frames until the answer to `id` arrives.
    async fn read_reply(&mut self, id: i64) -> Result<Value, ClientError> {
        loop {
            while let Some(value) = self.pending.pop_front() {
                match classify(&value, id) {
                    Frame::Ours => return Ok(value),
                    // Kept rather than dropped: a client that bootstrapped is
                    // owed every event, and one that arrives while a call is in
                    // flight is the normal case, not an oddity.
                    Frame::Ignorable => self.notifications.push_back(value),
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

/// Whether one frame is a server-initiated notification: a method, no id.
fn is_notification(value: &Value) -> bool {
    value.get("method").is_some()
        && matches!(value.get("id"), None | Some(Value::Null))
        && value.get("error").is_none()
}

/// What one decoded frame is, relative to the call waiting for an answer.
enum Frame {
    /// The answer to the outstanding call.
    Ours,
    /// A server-initiated notification, buffered for
    /// [`Connection::next_notification`].
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

    /// A daemon that accepts the call and never answers is a typed timeout
    /// after the caller's budget, not a hang.
    #[tokio::test]
    async fn a_call_that_goes_unanswered_times_out_at_the_callers_budget() {
        let dir = std::env::temp_dir().join(format!("mp-client-timeout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a scratch dir");
        let socket = dir.join("silent.sock");
        let _ = std::fs::remove_file(&socket);
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind");
        let silent = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            // Hold the connection open and say nothing.
            tokio::time::sleep(Duration::from_secs(5)).await;
            drop(stream);
        });

        let mut conn = Connection::connect(&socket).await.expect("connect");
        let started = std::time::Instant::now();
        let error = conn
            .call_within("account.list", json!({}), Duration::from_millis(100))
            .await
            .expect_err("nothing answers");
        assert!(
            matches!(&error, ClientError::Timeout { method, after }
                if method == "account.list" && *after == Duration::from_millis(100)),
            "got {error:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(error.to_string(), "account.list went unanswered for 0s");
        silent.abort();
    }

    /// A listener at a fresh socket path, for one test.
    fn listener(name: &str) -> (tokio::net::UnixListener, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("mp-client-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a scratch dir");
        let socket = dir.join("d.sock");
        let _ = std::fs::remove_file(&socket);
        (
            tokio::net::UnixListener::bind(&socket).expect("bind"),
            socket,
        )
    }

    /// A daemon that answers the first request late: after the caller's budget
    /// ran out, and with the id that call was given. The timed-out connection
    /// is closed, so the next call is `Closed` rather than a protocol error
    /// over the stale reply, and the daemon sees the disconnect at once.
    #[tokio::test]
    async fn a_timed_out_call_closes_the_connection_instead_of_leaving_the_reply_owed() {
        let (listener, socket) = listener("poison");
        let (saw_eof_tx, saw_eof_rx) = tokio::sync::oneshot::channel();
        let late = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut buf = vec![0u8; 4096];
            let read = stream.read(&mut buf).await.expect("the request");
            assert!(read > 0, "the request arrived");
            tokio::time::sleep(Duration::from_millis(200)).await;
            // The late reply to id 1; the client has shut its write half, so
            // the write may or may not land, and either is fine.
            let _ = stream
                .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n")
                .await;
            // What the daemon sees next is the end of the stream, not a
            // second request.
            let next = stream.read(&mut buf).await.unwrap_or(0);
            let _ = saw_eof_tx.send(next);
        });

        let mut conn = Connection::connect(&socket).await.expect("connect");
        let error = conn
            .call_within("account.list", json!({}), Duration::from_millis(50))
            .await
            .expect_err("the answer is late");
        assert!(
            matches!(error, ClientError::Timeout { .. }),
            "got {error:?}"
        );
        assert!(conn.is_closed());

        tokio::time::sleep(Duration::from_millis(300)).await;
        let error = conn
            .call("account.list", json!({}))
            .await
            .expect_err("a closed connection answers nothing");
        assert!(matches!(error, ClientError::Closed), "got {error:?}");
        let error = conn
            .call_within("account.list", json!({}), Duration::from_secs(1))
            .await
            .expect_err("and keeps answering nothing");
        assert!(matches!(error, ClientError::Closed), "got {error:?}");
        assert!(conn.next_notification().await.is_none());

        let next = tokio::time::timeout(Duration::from_secs(2), saw_eof_rx)
            .await
            .expect("the daemon side finished")
            .expect("it reported");
        assert_eq!(next, 0, "the daemon read end-of-stream after the timeout");
        late.await.expect("the fake daemon did not panic");
    }

    /// A plain `call` whose future the caller dropped under its own timeout
    /// poisons the connection the same way: the next call is `Closed` and
    /// never reads the abandoned call's reply as its own.
    #[tokio::test]
    async fn a_call_abandoned_by_an_outer_timeout_closes_the_connection() {
        let (listener, socket) = listener("abandon");
        let late = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut buf = vec![0u8; 4096];
            let _ = stream.read(&mut buf).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = stream
                .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n")
                .await;
            tokio::time::sleep(Duration::from_secs(2)).await;
        });

        let mut conn = Connection::connect(&socket).await.expect("connect");
        let abandoned = tokio::time::timeout(
            Duration::from_millis(30),
            conn.call("account.list", json!({})),
        )
        .await;
        assert!(abandoned.is_err(), "the outer timeout fired");
        assert!(conn.is_closed());

        tokio::time::sleep(Duration::from_millis(200)).await;
        let error = conn
            .call("account.list", json!({}))
            .await
            .expect_err("the abandoned call poisoned the connection");
        assert!(matches!(error, ClientError::Closed), "got {error:?}");
        late.abort();
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
