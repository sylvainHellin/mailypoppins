//! Connect, subscribe, start an operation, settle it: the sequence a one-shot
//! client opens every durable command with, over one [`Connection`].
//!
//! Lifted from `src/main.rs` (`daemon_connection`, `operation_session`,
//! `run_admin_operation`, `settle`, `await_operation`) so a second consumer
//! does not reinvent it. What stayed in the binary is policy: where the socket
//! is, whether to start a daemon on demand, which budget each method gets, and
//! what a failure prints and exits with.
//!
//! The order matters and is the protocol's: a connection receives
//! `operation.progress` and `operation.finished` only after it subscribed,
//! which `state.bootstrap` is ([`Connection::subscribe_within`]), so a client
//! that started an operation first would wait for a notification nobody
//! addressed to it.
//!
//! A long-lived client with a [`Session`](crate::session::Session) does not
//! use this: its session thread already reads every notification, and it
//! settles an operation from the event stream instead.

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

pub use mp_protocol::events::{KIND_OPERATION_FINISHED, KIND_OPERATION_PROGRESS};

use crate::types::{ClientError, ClientInfo, Identity, InitializeResult};
use crate::Connection;

/// How one operation ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Settled {
    /// The `result` a succeeded operation produced.
    Done(Value),
    /// The message a failed or cancelled operation stopped with.
    Failed(String),
}

impl Connection {
    /// Connect to `socket` and complete the `initialize` handshake, both under
    /// `budget`.
    ///
    /// [`ClientError::NotRunning`] when nothing listens there, which is the one
    /// failure a caller answers by starting a daemon; [`ClientError::Timeout`]
    /// (method `initialize`) when the handshake did not complete in time.
    pub async fn open(
        socket: &Path,
        info: ClientInfo,
        identity: Identity,
        budget: Duration,
    ) -> Result<(Connection, InitializeResult), ClientError> {
        let handshake = async {
            let mut connection = Connection::connect(socket).await?;
            let hello = connection.initialize(info, identity, &[], &[]).await?;
            Ok::<_, ClientError>((connection, hello))
        };
        match tokio::time::timeout(budget, handshake).await {
            Ok(opened) => opened,
            Err(_) => Err(ClientError::Timeout {
                method: "initialize".to_string(),
                after: budget,
            }),
        }
    }

    /// Subscribe this connection to events by bootstrapping it, and answer the
    /// snapshot as the daemon sent it.
    ///
    /// Raw rather than decoded into `mp_protocol::state::Bootstrap`, because a
    /// one-shot command bootstraps to subscribe and reads nothing of the
    /// snapshot; a caller that wants it typed decodes the value.
    pub async fn subscribe_within(&mut self, budget: Duration) -> Result<Value, ClientError> {
        self.call_within("state.bootstrap", json!({}), budget).await
    }

    /// Follow the operation `id` to its end, handing each `operation.progress`
    /// payload to `on_progress` on the way.
    ///
    /// Events rather than polling: a progress report lives on
    /// `operation.progress`, and a poll of `operation.status` only ever sees
    /// the newest one. Both kinds are lifecycle events, so neither is coalesced
    /// away nor dropped when a slow client overflows its queue. There is no
    /// budget: a full sync takes as long as the mailbox does. Notifications
    /// about other operations and other kinds are skipped.
    ///
    /// [`ClientError::Closed`] when the daemon closes the connection first.
    pub async fn await_operation(
        &mut self,
        id: &str,
        mut on_progress: impl FnMut(&Value),
    ) -> Result<Settled, ClientError> {
        loop {
            let Some(notification) = self.next_notification().await else {
                return Err(ClientError::Closed);
            };
            let params = notification.params;
            if params["payload"]["operation_id"].as_str() != Some(id) {
                continue;
            }
            match params["kind"].as_str() {
                Some(KIND_OPERATION_PROGRESS) => on_progress(&params["payload"]),
                Some(KIND_OPERATION_FINISHED) => {
                    let payload = &params["payload"];
                    return Ok(match payload["state"].as_str() {
                        Some("succeeded") => Settled::Done(payload["result"].clone()),
                        _ => Settled::Failed(
                            payload["error"]["message"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string(),
                        ),
                    });
                }
                _ => {}
            }
        }
    }

    /// Settle the operation a call answered `{operation_id}` for, with no
    /// progress to render.
    pub async fn settle(&mut self, started: &Value) -> Result<Settled, ClientError> {
        let id = started["operation_id"].as_str().unwrap_or_default();
        self.await_operation(id, |_| {}).await
    }

    /// Start one operation-kind method under `budget` and follow it to its
    /// end: the call, then [`Connection::await_operation`].
    ///
    /// The daemon's refusal to start it is [`ClientError::Rpc`], exactly as a
    /// plain call's; a failure after it started is [`Settled::Failed`].
    pub async fn run_operation(
        &mut self,
        method: &str,
        params: Value,
        budget: Duration,
        on_progress: impl FnMut(&Value),
    ) -> Result<Settled, ClientError> {
        let started = self.call_within(method, params, budget).await?;
        let id = started["operation_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        self.await_operation(&id, on_progress).await
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    use super::*;

    /// A socket in a scratch directory of its own.
    fn socket(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mp-client-op-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a scratch dir");
        let socket = dir.join("d.sock");
        let _ = std::fs::remove_file(&socket);
        socket
    }

    /// A stand-in daemon that answers `state.bootstrap`, then `sync.full` with
    /// an operation id, then publishes two unrelated events, a progress report
    /// and the finish, in that order, and closes.
    async fn scripted(listener: UnixListener) {
        let (stream, _) = listener.accept().await.expect("accept");
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        let send = |value: Value| {
            let mut line = serde_json::to_vec(&value).expect("json");
            line.push(b'\n');
            line
        };
        for _ in 0..2 {
            let request: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            let result = match request["method"].as_str() {
                Some("state.bootstrap") => json!({"revision": 1}),
                _ => json!({"operation_id": "op-1"}),
            };
            let frame = send(json!({"jsonrpc": "2.0", "id": request["id"], "result": result}));
            write.write_all(&frame).await.unwrap();
        }
        let event = |kind: &str, payload: Value| {
            json!({"jsonrpc": "2.0", "method": "state.event",
                   "params": {"instance_id": "i", "revision": 2, "kind": kind, "payload": payload}})
        };
        for value in [
            event(
                "operation.finished",
                json!({"operation_id": "other", "state": "succeeded"}),
            ),
            event("sync.completed", json!({"account": "a"})),
            event(
                "operation.progress",
                json!({"operation_id": "op-1", "done": 1}),
            ),
            event(
                "operation.finished",
                json!({"operation_id": "op-1", "state": "succeeded", "result": {"n": 3}}),
            ),
        ] {
            let frame = send(value);
            write.write_all(&frame).await.unwrap();
        }
    }

    #[tokio::test]
    async fn subscribe_start_and_settle_follow_only_their_own_operation() {
        let path = socket("settle");
        let daemon = tokio::spawn(scripted(UnixListener::bind(&path).expect("bind")));
        let mut conn = Connection::connect(&path).await.expect("connect");

        let snapshot = conn
            .subscribe_within(Duration::from_secs(5))
            .await
            .expect("bootstrap");
        assert_eq!(snapshot["revision"], json!(1));

        let mut progress = Vec::new();
        let settled = conn
            .run_operation("sync.full", json!({}), Duration::from_secs(5), |p| {
                progress.push(p["done"].clone())
            })
            .await
            .expect("settled");
        assert_eq!(settled, Settled::Done(json!({"n": 3})));
        assert_eq!(
            progress,
            vec![json!(1)],
            "another operation's events are skipped"
        );
        daemon.await.expect("the script ran");
    }

    #[tokio::test]
    async fn a_daemon_that_closes_mid_operation_is_a_closed_error() {
        let path = socket("closed");
        let listener = UnixListener::bind(&path).expect("bind");
        let daemon = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            drop(stream);
        });
        let mut conn = Connection::connect(&path).await.expect("connect");
        daemon.await.expect("closed");
        let error = conn
            .await_operation("op-1", |_| {})
            .await
            .expect_err("nothing will finish");
        assert!(matches!(error, ClientError::Closed), "got {error:?}");
    }

    #[tokio::test]
    async fn opening_nothing_is_not_running() {
        let path = socket("absent");
        let error = Connection::open(
            &path,
            ClientInfo {
                kind: crate::ClientKind::Gui,
                app_version: "0".to_string(),
            },
            Identity {
                data_dir: "/d".into(),
                config_dir: "/c".into(),
            },
            Duration::from_secs(1),
        )
        .await
        .expect_err("nothing listens");
        assert!(matches!(error, ClientError::NotRunning), "got {error:?}");
    }
}
