//! What a caller hands the handshake, what it gets back, and how it fails.
//!
//! These are Rust-side names; the wire names they map onto are pinned by
//! `crates/mp-protocol/fixtures/initialize.{request,response}.json` and applied
//! in [`crate::Connection::initialize`]. They differ in one place: the daemon's
//! application version arrives as `daemon.version` and is exposed as
//! [`InitializeResult::app_version`], mirroring [`ClientInfo::app_version`],
//! which travels as `client.version`.

use std::path::PathBuf;

use serde_json::Value;

use mp_protocol::RpcError;

/// Which kind of client is connecting, for the daemon's logs and for the
/// policies that differ between a one-shot CLI and a long-lived GUI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClientKind {
    /// A one-shot command-line invocation.
    Cli,
    /// The terminal user interface.
    Tui,
    /// A native desktop shell.
    Gui,
}

impl ClientKind {
    /// The `client.type` string this kind travels as.
    pub fn as_str(self) -> &'static str {
        match self {
            ClientKind::Cli => "cli",
            ClientKind::Tui => "tui",
            ClientKind::Gui => "gui",
        }
    }
}

/// How a client identifies itself in the handshake.
#[derive(Clone, Debug)]
pub struct ClientInfo {
    /// The client kind, sent as `client.type`.
    pub kind: ClientKind,
    /// The client's own application version, sent as `client.version`.
    pub app_version: String,
}

/// The directory pair a client and a daemon must agree on.
///
/// Sent unconditionally rather than only when an override is active:
/// `MAILYPOPPINS_DATA_DIR` and `MAILYPOPPINS_CONFIG_DIR` are independent, so a
/// client with one of them set would otherwise reach a daemon holding other
/// config, secrets, and signatures.
#[derive(Clone, Debug)]
pub struct Identity {
    /// The client's resolved data directory.
    pub data_dir: PathBuf,
    /// The client's resolved config directory.
    pub config_dir: PathBuf,
}

/// Host facts a client cannot infer from its own process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlatformInfo {
    /// The daemon host's OS, as `std::env::consts::OS` names it.
    pub os: String,
    /// The transport in use, `"unix_socket"` on every Phase 2 connection.
    pub transport: String,
}

/// The state of the configuration on disk, as the daemon found it. A daemon
/// serves in all three: an absent or invalid configuration is zero accounts and
/// a diagnostic, never a refused connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigStatus {
    /// No `config.toml` exists yet.
    Absent,
    /// The configuration loaded, with this many accounts.
    Loaded {
        /// Number of configured accounts.
        accounts: usize,
    },
    /// The configuration exists but does not load; carries the daemon's first
    /// problem, which is what a client shows the user.
    Invalid {
        /// The first entry of the daemon's `config_status.problems`.
        message: String,
    },
}

/// What a successful `initialize` reports back.
#[derive(Clone, Debug)]
pub struct InitializeResult {
    /// The daemon's application version, from `daemon.version`.
    pub app_version: String,
    /// The protocol version both sides will speak, from `protocol.selected`.
    pub protocol: u32,
    /// The daemon process this connection reached; a change means a restart.
    pub instance_id: String,
    /// The capability identifiers the daemon offers, in the daemon's order.
    pub capabilities: Vec<String>,
    /// Host and transport facts.
    pub platform: PlatformInfo,
    /// The configuration state on disk.
    pub config: ConfigStatus,
    /// The daemon's `lifecycle` object, verbatim: the fields it grows are
    /// additive, and Phase 2 clients ignore all of them.
    pub lifecycle: Value,
}

/// Everything a call can fail with. [`ClientError::Rpc`] carries the daemon's
/// error unchanged, code, message and `data` as they arrived, so a caller
/// matches on [`mp_protocol::ErrorCode::from_code`], never on message text.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The daemon answered with a JSON-RPC error.
    #[error("the daemon refused the call: {} ({})", .0.message, .0.code)]
    Rpc(RpcError),
    /// The socket failed underneath us.
    #[error("daemon socket I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// Nothing is listening on the socket path. Distinct from
    /// [`ClientError::Io`] because it is the one failure a caller answers by
    /// starting a daemon rather than by reporting a fault.
    #[error("no daemon is listening on that socket")]
    NotRunning,
    /// The daemon answered, but not with something this protocol allows.
    #[error("the daemon broke the protocol: {0}")]
    Protocol(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_kinds_travel_as_the_fixture_spells_them() {
        assert_eq!(ClientKind::Cli.as_str(), "cli");
        assert_eq!(ClientKind::Tui.as_str(), "tui");
        assert_eq!(ClientKind::Gui.as_str(), "gui");
    }

    #[test]
    fn a_missing_daemon_is_not_an_io_fault() {
        let error = ClientError::NotRunning;
        assert!(error.to_string().contains("no daemon"));
    }
}
