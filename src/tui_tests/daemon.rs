//! An in-process daemon for the TUI's own tests (P5-U6, #0124).
//!
//! The fixture pattern P5-U1 established and P5-U3, P5-U5 and P5-U6 reused,
//! extracted here the moment a second module needed it: a
//! [`Dispatcher`](crate::daemon::dispatch::Dispatcher) over a fixture data root,
//! reachable either as a [`Queries`] (for a layer function that takes the door
//! as an argument) or as a [`Session`] an [`App`](crate::tui::app::App) can own
//! (for an action helper that reads the door off `app.session`).
//!
//! Being a real dispatcher rather than a JSON mock is the whole point: the
//! layer under test is pinned against the daemon's real method bodies, over a
//! store the test seeded, so an answer that agrees with nobody fails here
//! rather than in a pty six months later.
//!
//! # The data root
//!
//! The caller owns it, because it owns the store it seeds and the paths it
//! asserts on: this takes the ambient one
//! ([`crate::config::mailypoppins_data_dir`], which a
//! [`TestDataDir`](crate::config::test_env::TestDataDir) has already pointed at
//! a tempdir) and re-installs it on every thread it starts. The override of
//! #0077 is thread-local and the mutation methods hop to `spawn_blocking`, so
//! without that a mutation resolves `store_path` against the developer's own
//! tree (`docs/lessons-learned.md`).

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use mp_protocol::{Request, RequestId, JSONRPC_VERSION};

use crate::config::{AccountConfig, GlobalConfig};
use crate::daemon::config::{ConfigState, ConfigStore};
use crate::daemon::dispatch::{ClientCtx, ClientKind};
use crate::daemon::runtime::InstanceMeta;
use crate::daemon::server::DaemonState;
use crate::tui::queries::Queries;
use crate::tui::session::Session;

/// A daemon serving the ambient fixture data root.
pub(super) struct TestDaemon {
    state: DaemonState,
    runtime: tokio::runtime::Runtime,
    root: PathBuf,
}

impl TestDaemon {
    /// A daemon over the ambient data root, configured with `accounts` in the
    /// order given, each with nothing but its name.
    pub(super) fn new(accounts: &[&str]) -> TestDaemon {
        let root = crate::config::mailypoppins_data_dir();
        let config = Arc::new(ConfigStore::new(
            root.join("config.toml"),
            ConfigState::Ok,
            GlobalConfig {
                accounts: accounts
                    .iter()
                    .map(|name| AccountConfig {
                        name: (*name).to_string(),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            },
            false,
        ));
        let state = DaemonState::new(
            InstanceMeta {
                app_version: "0.0.0-p5u6".to_string(),
                protocol_min: mp_protocol::PROTOCOL_MIN,
                protocol_max: mp_protocol::PROTOCOL_MAX,
                instance_id: "tui-test".to_string(),
                pid: 42,
                started_at: "2026-07-28T09:00:00Z".to_string(),
                data_dir: root.clone(),
                config_dir: root.clone(),
            },
            config,
        );
        let runtime = new_runtime(&root);
        TestDaemon {
            state,
            runtime,
            root,
        }
    }

    /// This daemon behind a [`Session`], for an `App` that reaches it through
    /// `app.session` and a [`QueryHandle`](crate::tui::session::QueryHandle).
    ///
    /// The daemon moves onto the session thread, which is the arrangement the
    /// real session has: the `App` owns the session, the session owns the
    /// connection, and the UI thread only ever holds a channel. The tempdir
    /// stays with the test, so the `App` (and with it the session) must be
    /// dropped before it, which declaration order already gives.
    pub(super) fn session(self) -> Session {
        let root = self.root.clone();
        Session::serving(move || {
            std::mem::forget(crate::config::test_env::DataDirOverride::set(&root));
            self
        })
    }
}

/// Every call a recording session forwarded, as `(method, params)` in order.
pub(super) type CallLog = Arc<std::sync::Mutex<Vec<(String, Value)>>>;

/// A [`TestDaemon`] that writes down each call before it answers it.
struct Recording {
    daemon: TestDaemon,
    log: CallLog,
}

impl Queries for Recording {
    fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.log
            .lock()
            .expect("the call log")
            .push((method.to_string(), params.clone()));
        self.daemon.call(method, params)
    }

    fn list_conversations(
        &self,
        account: &str,
        mailbox: &str,
    ) -> anyhow::Result<mp_protocol::listing::ConversationListing> {
        self.log.lock().expect("the call log").push((
            mp_protocol::listing::METHOD_MESSAGE_LIST_CONVERSATIONS.to_string(),
            serde_json::json!({"account": account, "mailbox": mailbox}),
        ));
        self.daemon.list_conversations(account, mailbox)
    }
}

impl TestDaemon {
    /// [`Self::session`], with every call the `App` makes through it written
    /// to `log` first: what a key handler that reads its door off
    /// `app.session` asked the daemon for, which a test asserts on.
    pub(super) fn recording_session(self, log: CallLog) -> Session {
        let root = self.root.clone();
        Session::serving(move || {
            std::mem::forget(crate::config::test_env::DataDirOverride::set(&root));
            Recording { daemon: self, log }
        })
    }
}

impl Queries for TestDaemon {
    fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let ctx = ClientCtx {
            connection_id: 1,
            kind: ClientKind::Tui,
            protocol: 1,
            capabilities: Vec::new(),
            rows: None,
        };
        let request = Request {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: Some(RequestId::Num(1)),
            method: method.to_string(),
            params,
        };
        let outcome = self
            .runtime
            .block_on(self.state.dispatcher.dispatch(&ctx, request))
            .map_err(|e| anyhow::anyhow!("{method}: {e}"))?;
        Ok(outcome.result)
    }

    /// `message.list_conversations` against the real method, its chunks
    /// collected off a row sink of this fixture's own (#0142).
    ///
    /// The method streams to the calling connection and refuses a caller with
    /// none, so this hands it the sink a connection's writer would drain and
    /// reads the `message.conversations` frames back in order until the
    /// answer's `total` is in. The flat stream needs nothing of the kind,
    /// since `message.list` answers it whole.
    fn list_conversations(
        &self,
        account: &str,
        mailbox: &str,
    ) -> anyhow::Result<mp_protocol::listing::ConversationListing> {
        use mp_protocol::listing::{
            ConversationListing, ConversationRowsChunk, MessageListStreamStarted,
            METHOD_MESSAGE_LIST_CONVERSATIONS,
        };
        let (sink, mut frames) = crate::daemon::dispatch::RowsSink::channel();
        let ctx = ClientCtx {
            connection_id: 1,
            kind: ClientKind::Tui,
            protocol: 1,
            capabilities: Vec::new(),
            rows: Some(sink),
        };
        let request = Request {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: Some(RequestId::Num(1)),
            method: METHOD_MESSAGE_LIST_CONVERSATIONS.to_string(),
            params: serde_json::json!({"account": account, "mailbox": mailbox}),
        };
        let outcome = self
            .runtime
            .block_on(self.state.dispatcher.dispatch(&ctx, request))
            .map_err(|e| anyhow::anyhow!("{METHOD_MESSAGE_LIST_CONVERSATIONS}: {e}"))?;
        let started: MessageListStreamStarted = serde_json::from_value(outcome.result)?;
        let mut conversations = Vec::new();
        while (conversations.len() as u64) < started.total {
            let frame = self
                .runtime
                .block_on(frames.recv())
                .ok_or_else(|| anyhow::anyhow!("the stream ended short of its total"))?;
            let line = std::str::from_utf8(&frame.bytes)?.trim_end();
            let chunk = ConversationRowsChunk::from_frame_line(line)
                .ok_or_else(|| anyhow::anyhow!("a frame that is no conversation chunk: {line}"))?;
            anyhow::ensure!(
                chunk.offset == conversations.len() as u64,
                "a chunk at {} after {} rows",
                chunk.offset,
                conversations.len()
            );
            conversations.extend(chunk.rows);
        }
        Ok(ConversationListing {
            account: started.account,
            mailbox: started.mailbox,
            total: started.total,
            conversations,
        })
    }
}

/// A runtime whose every thread resolves paths under `root`.
///
/// **Multi-threaded, and that is load-bearing.** An operation method answers
/// `{operation_id}` and runs its work on a `tokio::spawn`ed task; on a
/// current-thread runtime that task is only driven while something is blocked
/// on it, and a caller polling `operation.status` in a loop never is, so the
/// operation would sit in `running` for ever. Two workers is enough for the
/// one operation a test drives.
///
/// `on_thread_start` for the reason in the module header: the override is
/// thread-local, and the guard is forgotten because the thread dies with the
/// runtime.
fn new_runtime(root: &std::path::Path) -> tokio::runtime::Runtime {
    let root = root.to_path_buf();
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .on_thread_start(move || {
            std::mem::forget(crate::config::test_env::DataDirOverride::set(&root));
        })
        .build()
        .expect("a multi-thread runtime")
}
