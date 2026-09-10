//! The read-only method family: `account.list`, `mailbox.list` and the
//! `message.*` read slice (P2-U11, P4-U4).
//!
//! These are the first domain methods the daemon serves, and they are reads
//! only. Both go through the same store path the CLI takes
//! ([`crate::store::read`]) and neither takes an [`crate::engine_lock::EngineLock`]:
//! the daemon does not become an account's engine before Phase 5, so a running
//! TUI or `mp sync` keeps the lock while the daemon answers listings beside it.
//!
//! Both are [`Query`](super::dispatch::MethodKind::Query) methods on the
//! [`Dispatcher`](super::dispatch::Dispatcher) the daemon builds at startup
//! (P3a-U2): they read, they change nothing, and their outcomes carry neither a
//! revision nor an affected resource. [`register`] is the single place that
//! says which methods this build serves; the handshake derives its capability
//! list from the same table, so a method cannot be served without being
//! advertised.
//!
//! Neither method reads [`DaemonState`](super::server::DaemonState): they take
//! the configured accounts, which is all the state a Phase 2 read needs, and
//! holding an `Arc` of that list is what keeps the dispatcher out of a cycle
//! with the state that owns it.

pub mod account;
pub mod calendar;
pub mod config;
pub mod contact;
pub mod diagnostic;
pub mod draft;
pub mod mailbox;
pub mod message;
pub mod send;
pub mod state;
pub mod sync;

use std::sync::Arc;

use serde_json::Value;

use mp_protocol::RpcError;

use super::config::ConfigStore;
use super::dispatch::Dispatcher;
use super::handles::HandleTable;
use super::operations::{
    fake_operations, OperationCancelMethod, OperationRegistry, OperationStatusMethod, TestOperation,
};
use super::server::RuntimeTable;
use super::state::{fake_ready_delay, CanonicalState};
use super::watch::DraftWatch;

/// JSON-RPC's own "invalid params".
const INVALID_PARAMS: i32 = -32602;
/// JSON-RPC's own "internal error".
const INTERNAL_ERROR: i32 = -32603;

/// Register every domain method this build serves.
///
/// The single place that says which methods exist: the handshake derives its
/// capability list from the same table, so a method cannot be served without
/// being advertised. `test.operation` is the one exception a build can add, and
/// only behind
/// [`FAKE_OPERATIONS_ENV`](super::operations::FAKE_OPERATIONS_ENV).
pub fn register(
    dispatcher: &mut Dispatcher,
    config: Arc<ConfigStore>,
    runtimes: Arc<RuntimeTable>,
    canonical: Arc<CanonicalState>,
    operations: Arc<OperationRegistry>,
    watch: Arc<DraftWatch>,
    handles: Arc<HandleTable>,
) {
    dispatcher.register(Arc::new(account::AccountList {
        config: Arc::clone(&config),
    }));
    dispatcher.register(Arc::new(mailbox::MailboxList {
        config: Arc::clone(&config),
    }));
    dispatcher.register(Arc::new(mailbox::MailboxListServer {
        config: Arc::clone(&config),
    }));
    message::register_reads(dispatcher, Arc::clone(&config));
    message::register_mutations(dispatcher, Arc::clone(&config), Arc::clone(&canonical));
    message::register_handles(dispatcher, Arc::clone(&config), handles);
    dispatcher.register(Arc::new(message::MessageListServer {
        config: Arc::clone(&config),
    }));
    self::sync::register(
        dispatcher,
        Arc::clone(&config),
        Arc::clone(&runtimes),
        Arc::clone(&canonical),
        Arc::clone(&operations),
    );
    self::draft::register(
        dispatcher,
        Arc::clone(&config),
        Arc::clone(&watch),
        Arc::clone(&canonical),
    );
    self::send::register(
        dispatcher,
        Arc::clone(&config),
        Arc::clone(&canonical),
        Arc::clone(&operations),
    );
    self::contact::register(dispatcher, Arc::clone(&config), Arc::clone(&operations));
    self::calendar::register(dispatcher, Arc::clone(&config), Arc::clone(&operations));
    self::diagnostic::register(dispatcher, Arc::clone(&config), Arc::clone(&operations));
    self::config::register(
        dispatcher,
        Arc::new(self::config::ConfigFamily {
            store: config,
            runtimes,
            canonical: Arc::clone(&canonical),
            watch,
            operations: Arc::clone(&operations),
        }),
    );
    dispatcher.register(Arc::new(self::state::StateBootstrap::new(
        Arc::clone(&canonical),
        fake_ready_delay(),
    )));
    dispatcher.register(Arc::new(OperationStatusMethod {
        registry: Arc::clone(&operations),
    }));
    dispatcher.register(Arc::new(OperationCancelMethod {
        registry: Arc::clone(&operations),
        canonical,
    }));
    if fake_operations() {
        dispatcher.register(Arc::new(TestOperation {
            registry: operations,
        }));
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

/// Refuse a call carrying a parameter the method does not take.
///
/// The admin slice needs this on every method: `contact.rebuild`,
/// `diagnostic.store_gc` and `config.cutover` all have an `--all-accounts`
/// form on the command line whose loop stays in the client, and a method that
/// quietly ignored an `all_accounts` it was sent would let that loop drift into
/// the daemon one release later.
fn only_params(method: &str, params: &Value, allowed: &[&str]) -> Result<(), RpcError> {
    let Some(object) = params.as_object() else {
        return Ok(());
    };
    match object.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(unexpected) => Err(invalid_params(format!(
            "{method} has no {unexpected} parameter; it takes {}",
            allowed.join(", ")
        ))),
        None => Ok(()),
    }
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

/// `-32603` naming the account whose server could not be reached (P4-U10).
///
/// Not `-32005` and not `-32006`: the account is configured, so either would
/// contradict what `account.list` says about it. Not `-32602` either: the
/// caller's parameters were right and the server or its credentials were not.
/// The message is the error verbatim, because it is the sentence the user has
/// always read and has to act on.
fn server_error(account: &str, error: &anyhow::Error) -> RpcError {
    RpcError {
        code: INTERNAL_ERROR,
        message: format!("{error}"),
        data: Some(serde_json::json!({"account": account})),
    }
}

/// The secrets backend, opened on first use rather than at startup: the same
/// rule `config.set_password` and the mutation slice follow, because a first run
/// has no configuration to select one from and the opener is idempotent.
fn open_secrets(
    account: &str,
    kind: crate::secrets::SecretsBackendKind,
) -> Result<(), RpcError> {
    crate::secrets::init(kind).map_err(|e| server_error(account, &anyhow::anyhow!("{e}")))
}
