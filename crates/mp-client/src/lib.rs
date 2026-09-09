//! Client transport for the mailypoppins daemon protocol (P2-U9).
//!
//! One [`Connection`] is one Unix-domain socket connection to the daemon,
//! carrying newline-framed JSON-RPC 2.0 as `crates/mp-protocol` defines it.
//! The crate owns the wire, the `initialize` handshake, and the typed errors a
//! caller branches on; it owns no policy, no paths, and no configuration, and
//! it never depends on the `mailypoppins` crate, so a GUI can link it alone.
//!
//! ```no_run
//! # async fn demo(socket: &std::path::Path) -> Result<(), mp_client::ClientError> {
//! use mp_client::{ClientInfo, ClientKind, Connection, Identity};
//! let info = ClientInfo { kind: ClientKind::Cli, app_version: "0.9.0".into() };
//! let id = Identity { data_dir: "/data".into(), config_dir: "/config".into() };
//! let mut conn = Connection::connect(socket).await?;
//! let hello = conn.initialize(info, id, &[], &[]).await?;
//! println!("daemon {} instance {}", hello.app_version, hello.instance_id);
//! # Ok(())
//! # }
//! ```

mod connection;
mod types;

pub use connection::Connection;
pub use types::{
    ClientError, ClientInfo, ClientKind, ConfigStatus, Identity, InitializeResult, PlatformInfo,
};

/// Largest response frame this client buffers, terminator included. Capped
/// separately from [`mp_protocol::MAX_REQUEST_BYTES`], because a listing
/// legitimately dwarfs the call that asked for it.
pub const MAX_RESPONSE_BYTES: usize = 16 << 20;
