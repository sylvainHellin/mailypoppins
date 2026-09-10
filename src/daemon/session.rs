//! Per-connection session state: the `initialize` handshake and the gate that
//! keeps every other method behind it (P2-U9).
//!
//! A [`Session`] lives as long as one connection and is owned by the task that
//! serves it, so nothing here is shared or locked. It answers three questions:
//! has this connection handshaken, may this method run yet, and what does
//! `initialize` reply.
//!
//! Two methods are exempt from the gate on purpose, and
//! [`LIFECYCLE_METHODS`] is the single list of them: `daemon.status` must
//! describe a daemon whose protocol range the caller cannot negotiate, and
//! `daemon.stop` must be able to end one. Both are lifecycle surface, neither
//! touches account data, and `mp daemon status` on an incompatible daemon is
//! the whole reason the exemption exists.

use std::path::{Path, PathBuf};

use log::info;
use serde_json::{json, Value};

use mp_protocol::{ErrorCode, RpcError, PROTOCOL_MAX, PROTOCOL_MIN};

use super::dispatch::{ClientCtx, ClientKind};
use super::server::DaemonState;

/// The methods that are reachable before a handshake and are not served by the
/// dispatcher, in the order clients see them.
pub const LIFECYCLE_METHODS: &[&str] = &["daemon.status", "daemon.stop"];

/// The capabilities this build advertises, in the order clients see them:
/// lifecycle first, then every method registered on the dispatcher.
///
/// Derived rather than listed, so a method cannot be served without being
/// advertised or advertised without being served. A client requiring one this
/// build does not have gets `capability_missing` at the handshake rather than a
/// method that fails at the first call.
pub fn capabilities(state: &DaemonState) -> Vec<String> {
    LIFECYCLE_METHODS
        .iter()
        .map(|name| name.to_string())
        .chain(
            state
                .dispatcher
                .specs()
                .into_iter()
                .map(|spec| spec.name.to_string()),
        )
        .collect()
}

/// Whether `method` is lifecycle surface, reachable before a handshake.
pub fn is_lifecycle_method(method: &str) -> bool {
    LIFECYCLE_METHODS.contains(&method)
}

/// One connection's handshake state.
#[derive(Debug)]
pub struct Session {
    /// This connection's id, which the [`ClientCtx`] carries into every call.
    connection_id: u64,
    /// The identity the client sent, once it has initialized.
    negotiated: Option<Negotiated>,
}

/// What a successful handshake settled.
#[derive(Clone, Debug)]
struct Negotiated {
    /// The protocol version both sides speak.
    protocol: u32,
    /// Which kind of client is on the other end, logged at the handshake and
    /// carried into every call the connection makes.
    client_kind: ClientKind,
    /// The capabilities the client declared and this build offers, required
    /// first, in the order they were declared.
    capabilities: Vec<String>,
}

impl Session {
    /// A connection that has not handshaken yet.
    pub fn new(connection_id: u64) -> Self {
        Session {
            connection_id,
            negotiated: None,
        }
    }

    /// What a domain method may know about this caller, once it has
    /// handshaken.
    pub fn client_ctx(&self) -> Option<ClientCtx> {
        let negotiated = self.negotiated.as_ref()?;
        Some(ClientCtx {
            connection_id: self.connection_id,
            kind: negotiated.client_kind,
            protocol: negotiated.protocol,
            capabilities: negotiated.capabilities.clone(),
        })
    }

    /// Whether `initialize` has succeeded on this connection.
    pub fn is_initialized(&self) -> bool {
        self.negotiated.is_some()
    }

    /// The refusal `method` earns before a handshake, if any.
    ///
    /// Per request, not per connection: the connection stays usable and the
    /// handshake that should have come first still works on it.
    pub fn gate(&self, method: &str) -> Option<RpcError> {
        if self.is_initialized() || method == "initialize" || is_lifecycle_method(method) {
            return None;
        }
        Some(rpc_error(
            ErrorCode::NotInitialized,
            "the connection has not completed initialize",
            json!({}),
        ))
    }

    /// Handle `initialize`, returning the result or the refusal.
    pub fn initialize(&mut self, params: &Value, state: &DaemonState) -> Result<Value, RpcError> {
        if self.is_initialized() {
            return Err(RpcError {
                code: INVALID_REQUEST,
                message: "this connection has already completed initialize".to_string(),
                data: None,
            });
        }

        let request = InitializeParams::parse(params)?;
        check_identity(&request, state)?;
        let protocol = select_protocol(&request)?;
        let offered = capabilities(state);
        check_capabilities(&request, &offered)?;

        let negotiated = self.negotiated.insert(Negotiated {
            protocol,
            client_kind: request.client_kind,
            capabilities: agreed_capabilities(&request, &offered),
        });
        info!(
            "[daemon] a {} client completed initialize at protocol version {}",
            negotiated.client_kind.as_str(),
            negotiated.protocol
        );

        Ok(json!({
            "daemon": {"version": state.meta.app_version},
            "protocol": {"selected": protocol},
            "instance_id": state.meta.instance_id,
            "capabilities": offered,
            "platform": {"os": std::env::consts::OS, "transport": "unix_socket"},
            "lifecycle": {
                // Phase 2 daemons run until they are stopped: no idle timer, no
                // restart flag, no last-client shutdown.
                "idle_shutdown_seconds": Value::Null,
                "restart_required": false,
                "shutdown_on_last_client": false,
            },
            "config_status": state.config.report().to_json(),
        }))
    }
}

/// JSON-RPC's own "invalid request", which a second `initialize` earns.
const INVALID_REQUEST: i32 = -32600;

/// JSON-RPC's own "invalid params".
const INVALID_PARAMS: i32 = -32602;

/// The `initialize` params, once they have been checked for shape.
struct InitializeParams {
    client_kind: ClientKind,
    data_dir: String,
    config_dir: String,
    protocol_min: u32,
    protocol_max: u32,
    required: Vec<String>,
    optional: Vec<String>,
}

impl InitializeParams {
    fn parse(params: &Value) -> Result<Self, RpcError> {
        // The protocol fixes exactly three client kinds, and a method that
        // branches on the caller may not be handed a fourth.
        let client_kind = ClientKind::from_wire(&string_at(params, "/client/type")?)
            .ok_or_else(|| invalid_params("/client/type"))?;
        let data_dir = string_at(params, "/identity/data_dir")?;
        let config_dir = string_at(params, "/identity/config_dir")?;
        let protocol_min = number_at(params, "/protocol/min")?;
        let protocol_max = number_at(params, "/protocol/max")?;
        let required = params
            .pointer("/capabilities/required")
            .map(|value| identifiers(value, "capabilities.required"))
            .transpose()?
            .unwrap_or_default();
        let optional = params
            .pointer("/capabilities/optional")
            .map(|value| identifiers(value, "capabilities.optional"))
            .transpose()?
            .unwrap_or_default();
        Ok(InitializeParams {
            client_kind,
            data_dir,
            config_dir,
            protocol_min,
            protocol_max,
            required,
            optional,
        })
    }
}

/// The capabilities in effect on a connection: those the client asked for and
/// this build offers, required first, each once.
///
/// An optional capability the daemon does not have is dropped rather than
/// refused, which is what makes it optional; a required one never reaches here,
/// because [`check_capabilities`] has already refused the handshake.
fn agreed_capabilities(request: &InitializeParams, offered: &[String]) -> Vec<String> {
    let mut agreed: Vec<String> = Vec::new();
    for wanted in request.required.iter().chain(request.optional.iter()) {
        if offered.contains(wanted) && !agreed.contains(wanted) {
            agreed.push(wanted.clone());
        }
    }
    agreed
}

/// Refuse a client whose directory pair is not this daemon's.
///
/// Compared canonically, because the client resolves its own paths and a
/// symlinked `/tmp` or a trailing component would otherwise read as a
/// mismatch. The refusal reports the paths as each side spelled them, which is
/// what the user has to act on.
fn check_identity(request: &InitializeParams, state: &DaemonState) -> Result<(), RpcError> {
    let same = canonical(Path::new(&request.data_dir)) == canonical(&state.meta.data_dir)
        && canonical(Path::new(&request.config_dir)) == canonical(&state.meta.config_dir);
    if same {
        return Ok(());
    }
    Err(rpc_error(
        ErrorCode::IdentityMismatch,
        "the client's data and config directories differ from the daemon's",
        json!({
            "daemon": {
                "data_dir": state.meta.data_dir.display().to_string(),
                "config_dir": state.meta.config_dir.display().to_string(),
            },
            "client": {"data_dir": request.data_dir, "config_dir": request.config_dir},
        }),
    ))
}

/// The highest version both sides can speak, or the refusal naming both ranges.
fn select_protocol(request: &InitializeParams) -> Result<u32, RpcError> {
    let low = request.protocol_min.max(PROTOCOL_MIN);
    let high = request.protocol_max.min(PROTOCOL_MAX);
    if low <= high {
        return Ok(high);
    }
    Err(rpc_error(
        ErrorCode::ProtocolIncompatible,
        "no protocol version is supported by both sides",
        json!({
            "daemon": {"min": PROTOCOL_MIN, "max": PROTOCOL_MAX},
            "client": {"min": request.protocol_min, "max": request.protocol_max},
        }),
    ))
}

/// Refuse a client requiring capabilities this build does not offer.
fn check_capabilities(request: &InitializeParams, offered: &[String]) -> Result<(), RpcError> {
    let missing: Vec<&String> = request
        .required
        .iter()
        .filter(|wanted| !offered.contains(wanted))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(rpc_error(
        ErrorCode::CapabilityMissing,
        "the daemon does not offer every required capability",
        json!({"missing": missing}),
    ))
}

/// The configuration state the handshake reports.
///
/// Derived once at startup, because Phase 2 loads the configuration once; the
/// config-reload unit turns this into a value the daemon recomputes.
#[derive(Clone, Debug)]
pub enum ConfigReport {
    /// No `config.toml` at [`crate::config::config_path`].
    Absent {
        /// Where the daemon looked.
        path: PathBuf,
    },
    /// It loaded, with this many accounts.
    Loaded {
        /// Where it was loaded from.
        path: PathBuf,
        /// How many accounts it configures.
        accounts: usize,
    },
    /// It exists but does not load; the daemon serves zero accounts and says
    /// why rather than refusing to start.
    Invalid {
        /// Where it was read from.
        path: PathBuf,
        /// The load failure, one line, first in `problems`.
        problem: String,
    },
}

impl ConfigReport {
    /// The `config_status` object of an `initialize` result.
    pub fn to_json(&self) -> Value {
        match self {
            ConfigReport::Absent { path } => json!({
                "state": "absent",
                "path": path.display().to_string(),
                "accounts": 0,
                "problems": [],
            }),
            ConfigReport::Loaded { path, accounts } => json!({
                "state": "ok",
                "path": path.display().to_string(),
                "accounts": accounts,
                "problems": [],
            }),
            ConfigReport::Invalid { path, problem } => json!({
                "state": "invalid",
                "path": path.display().to_string(),
                "accounts": 0,
                "problems": [problem],
            }),
        }
    }
}

/// A required string parameter, or `-32602` naming it.
fn string_at(params: &Value, pointer: &str) -> Result<String, RpcError> {
    params
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| invalid_params(pointer))
}

/// A required unsigned parameter, or `-32602` naming it.
fn number_at(params: &Value, pointer: &str) -> Result<u32, RpcError> {
    params
        .pointer(pointer)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| invalid_params(pointer))
}

/// A list of identifier strings, or `-32602` naming the list.
fn identifiers(value: &Value, label: &str) -> Result<Vec<String>, RpcError> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| item.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
        })
        .ok_or_else(|| invalid_params(label))?
        .ok_or_else(|| invalid_params(label))
}

/// `-32602` for a parameter that is missing or of the wrong type.
fn invalid_params(what: &str) -> RpcError {
    RpcError {
        code: INVALID_PARAMS,
        message: format!(
            "initialize needs {} as the protocol spells it",
            what.trim_start_matches('/').replace('/', ".")
        ),
        data: None,
    }
}

/// One daemon-range error, with the payload its code fixes.
fn rpc_error(code: ErrorCode, message: &str, data: Value) -> RpcError {
    RpcError {
        code: code.code(),
        message: message.to_string(),
        data: Some(data),
    }
}

/// An absolute, symlink-resolved path, falling back to the path itself.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::runtime::InstanceMeta;

    fn state() -> DaemonState {
        DaemonState::new(
            InstanceMeta {
                app_version: "0.0.0-test".to_string(),
                protocol_min: PROTOCOL_MIN,
                protocol_max: PROTOCOL_MAX,
                instance_id: "abcd".to_string(),
                pid: 42,
                started_at: "2026-01-01T00:00:00Z".to_string(),
                data_dir: PathBuf::from("/tmp/mp-test-data"),
                config_dir: PathBuf::from("/tmp/mp-test-config"),
            },
            std::sync::Arc::new(crate::daemon::config::ConfigStore::new(
                PathBuf::from("/tmp/mp-test-config/config.toml"),
                crate::daemon::config::ConfigState::Absent,
                crate::config::GlobalConfig::default(),
                false,
            )),
        )
    }

    fn params(min: u32, max: u32, required: &[&str]) -> Value {
        params_with(min, max, required, &[])
    }

    fn params_with(min: u32, max: u32, required: &[&str], optional: &[&str]) -> Value {
        json!({
            "client": {"type": "tui", "version": "0.9.0"},
            "protocol": {"min": min, "max": max},
            "capabilities": {"required": required, "optional": optional},
            "identity": {"data_dir": "/tmp/mp-test-data", "config_dir": "/tmp/mp-test-config"},
        })
    }

    #[test]
    fn a_compatible_handshake_reports_the_daemon() {
        let mut session = Session::new(3);
        let result = session
            .initialize(&params(1, 1, &[]), &state())
            .expect("a compatible handshake succeeds");
        assert_eq!(result["daemon"]["version"], json!("0.0.0-test"));
        assert_eq!(result["protocol"]["selected"], json!(PROTOCOL_MAX));
        assert_eq!(result["instance_id"], json!("abcd"));
        assert_eq!(result["platform"]["transport"], json!("unix_socket"));
        assert_eq!(result["config_status"]["state"], json!("absent"));
        assert!(session.is_initialized());
    }

    #[test]
    fn the_gate_exempts_initialize_and_the_lifecycle_methods_only() {
        let session = Session::new(1);
        assert!(session.gate("initialize").is_none());
        assert!(session.gate("daemon.status").is_none());
        assert!(session.gate("daemon.stop").is_none());
        let refusal = session.gate("account.list").expect("gated");
        assert_eq!(refusal.code, ErrorCode::NotInitialized.code());
        assert_eq!(refusal.data, Some(json!({})));
    }

    #[test]
    fn an_initialized_session_gates_nothing() {
        let mut session = Session::new(1);
        session
            .initialize(&params(1, 1, &[]), &state())
            .expect("ok");
        assert!(session.gate("account.list").is_none());
    }

    /// The capability list is the dispatcher's table with the lifecycle
    /// methods in front of it, so nothing can be served unadvertised.
    #[test]
    fn the_advertised_capabilities_are_the_lifecycle_methods_and_the_table() {
        assert_eq!(
            capabilities(&state()),
            vec![
                "daemon.status".to_string(),
                "daemon.stop".to_string(),
                "account.list".to_string(),
                "config.add_account".to_string(),
                "config.get".to_string(),
                "config.init".to_string(),
                "config.reload".to_string(),
                "config.set_password".to_string(),
                "config.validate".to_string(),
                "draft.approve".to_string(),
                "draft.create".to_string(),
                "draft.demote".to_string(),
                "draft.discard".to_string(),
                "draft.forward".to_string(),
                "draft.list".to_string(),
                "draft.path".to_string(),
                "draft.preview".to_string(),
                "draft.reply".to_string(),
                "draft.validate".to_string(),
                "mailbox.list".to_string(),
                "mailbox.list_server".to_string(),
                "message.archive".to_string(),
                "message.delete".to_string(),
                "message.get".to_string(),
                "message.list".to_string(),
                "message.list_server".to_string(),
                "message.materialise_attachment".to_string(),
                "message.materialise_html".to_string(),
                "message.release_handle".to_string(),
                "message.search".to_string(),
                "operation.cancel".to_string(),
                "operation.status".to_string(),
                "send.approved".to_string(),
                "send.draft".to_string(),
                "send.invite".to_string(),
                "send.outbox_discard".to_string(),
                "send.outbox_list".to_string(),
                "send.outbox_retry".to_string(),
                "state.bootstrap".to_string(),
                "sync.full".to_string(),
                "sync.quick".to_string(),
                "sync.watch".to_string(),
            ]
        );
    }

    /// A handshaken connection hands methods its id, kind, protocol and the
    /// capabilities both sides agreed on; an unhandshaken one has no context.
    #[test]
    fn the_client_context_carries_what_the_handshake_settled() {
        let mut session = Session::new(7);
        assert!(session.client_ctx().is_none());
        session
            .initialize(
                &params_with(1, 1, &["account.list"], &["message.list", "calendar.rsvp"]),
                &state(),
            )
            .expect("ok");
        let ctx = session.client_ctx().expect("initialized");
        assert_eq!(ctx.connection_id, 7);
        assert_eq!(ctx.kind, ClientKind::Tui);
        assert_eq!(ctx.protocol, PROTOCOL_MAX);
        assert_eq!(
            ctx.capabilities,
            vec!["account.list".to_string(), "message.list".to_string()],
            "an optional capability this build lacks is dropped, not refused"
        );
    }

    /// The protocol fixes three client kinds, and a fourth is a bad parameter.
    #[test]
    fn an_unknown_client_type_is_invalid_params() {
        let mut params = params(1, 1, &[]);
        params["client"]["type"] = json!("robot");
        let error = Session::new(1)
            .initialize(&params, &state())
            .expect_err("refused");
        assert_eq!(error.code, INVALID_PARAMS);
    }

    #[test]
    fn a_second_initialize_is_an_invalid_request() {
        let mut session = Session::new(1);
        session
            .initialize(&params(1, 1, &[]), &state())
            .expect("ok");
        let error = session
            .initialize(&params(1, 1, &[]), &state())
            .expect_err("refused");
        assert_eq!(error.code, INVALID_REQUEST);
    }

    #[test]
    fn a_differing_directory_pair_names_all_four_directories() {
        let mut params = params(1, 1, &[]);
        params["identity"]["config_dir"] = json!("/tmp/mp-test-elsewhere");
        let error = Session::new(1)
            .initialize(&params, &state())
            .expect_err("refused");
        assert_eq!(error.code, ErrorCode::IdentityMismatch.code());
        let data = error.data.expect("identity_mismatch carries data");
        assert_eq!(data["daemon"]["config_dir"], json!("/tmp/mp-test-config"));
        assert_eq!(
            data["client"]["config_dir"],
            json!("/tmp/mp-test-elsewhere")
        );
        assert_eq!(data["daemon"]["data_dir"], json!("/tmp/mp-test-data"));
        assert_eq!(data["client"]["data_dir"], json!("/tmp/mp-test-data"));
    }

    #[test]
    fn a_disjoint_range_names_both_ranges() {
        let error = Session::new(1)
            .initialize(&params(2, 2, &[]), &state())
            .expect_err("refused");
        assert_eq!(error.code, ErrorCode::ProtocolIncompatible.code());
        let data = error.data.expect("protocol_incompatible carries data");
        assert_eq!(data["client"], json!({"min": 2, "max": 2}));
        assert_eq!(
            data["daemon"],
            json!({"min": PROTOCOL_MIN, "max": PROTOCOL_MAX})
        );
    }

    #[test]
    fn an_overlapping_range_selects_the_highest_shared_version() {
        let result = Session::new(1)
            .initialize(&params(1, 7, &[]), &state())
            .expect("a range containing ours overlaps");
        assert_eq!(result["protocol"]["selected"], json!(PROTOCOL_MAX));
    }

    #[test]
    fn a_missing_required_capability_lists_exactly_what_is_missing() {
        let error = Session::new(1)
            .initialize(
                &params(1, 1, &["daemon.status", "no.such.capability"]),
                &state(),
            )
            .expect_err("refused");
        assert_eq!(error.code, ErrorCode::CapabilityMissing.code());
        let data = error.data.expect("capability_missing carries data");
        assert_eq!(data["missing"], json!(["no.such.capability"]));
    }

    #[test]
    fn params_without_an_identity_are_invalid_params() {
        let error = Session::new(1)
            .initialize(&json!({"client": {"type": "cli"}}), &state())
            .expect_err("refused");
        assert_eq!(error.code, INVALID_PARAMS);
    }

    #[test]
    fn the_three_config_reports_serialise_as_the_protocol_spells_them() {
        let path = PathBuf::from("/c/config.toml");
        let absent = ConfigReport::Absent { path: path.clone() }.to_json();
        assert_eq!(absent["state"], json!("absent"));
        assert_eq!(absent["accounts"], json!(0));

        let loaded = ConfigReport::Loaded {
            path: path.clone(),
            accounts: 2,
        }
        .to_json();
        assert_eq!(loaded["state"], json!("ok"));
        assert_eq!(loaded["accounts"], json!(2));

        let invalid = ConfigReport::Invalid {
            path,
            problem: "line 4: unknown backend".to_string(),
        }
        .to_json();
        assert_eq!(invalid["state"], json!("invalid"));
        assert_eq!(invalid["problems"][0], json!("line 4: unknown backend"));
    }
}
