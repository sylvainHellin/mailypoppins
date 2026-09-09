//! Wire types and framing for the mailypoppins daemon protocol.
//!
//! The daemon speaks JSON-RPC 2.0 over a Unix-domain socket, one message per
//! line, UTF-8, `\n` terminated. This crate owns the message structs, the
//! numeric error table, the newline framing codec, and the event envelope; it
//! knows nothing about sockets, accounts, or the store.
//!
//! `docs/daemon-protocol.md` is the prose contract, and
//! `crates/mp-protocol/fixtures/*.json` pins every public shape on disk.
//! Changing a code, a field name, or the version range needs an entry in that
//! document's protocol changelog.

pub mod frame;

mod error;
mod message;

pub use error::ErrorCode;
pub use message::{
    ErrorResponse, EventEnvelope, Notification, Request, RequestId, Response, RpcError,
};

/// Lowest protocol version this build can speak.
pub const PROTOCOL_MIN: u32 = 1;

/// Highest protocol version this build can speak.
pub const PROTOCOL_MAX: u32 = 1;

/// Largest request frame the daemon accepts, terminator included: 1 MiB.
///
/// Responses carry their own cap, decided separately, so
/// [`frame::Decoder::new`] takes the limit rather than reading this constant.
pub const MAX_REQUEST_BYTES: usize = 1 << 20;

/// Largest response frame the protocol allows, terminator included: 16 MiB.
///
/// Sixteen times the request cap, because a listing legitimately dwarfs the
/// call that asked for it. Both sides read this one constant: the daemon
/// refuses to write a reply above it with `frame_too_large`, and a client sizes
/// its decoder by it, so an oversized answer is a named error on both ends
/// rather than a truncated frame on one and a closed connection on the other.
pub const MAX_RESPONSE_BYTES: usize = 16 << 20;

/// Method of the notification that carries an [`EventEnvelope`] as its params.
pub const METHOD_STATE_EVENT: &str = "state.event";

/// Method of the control notification that tells a client to re-bootstrap.
pub const METHOD_STATE_RESYNC_REQUIRED: &str = "state.resync_required";

/// The JSON-RPC version string every message on this protocol declares.
pub const JSONRPC_VERSION: &str = "2.0";
