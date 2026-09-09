//! The long-lived local daemon: runtime files, the startup lock, and (from
//! later units) the socket server and its lifecycle commands.
//!
//! The whole module is gated behind the `daemon` cargo feature during the
//! migration described in `.agents/workflow/native-gui-daemon/plan.md`; the
//! gate goes away in P4-U1, when clients require a daemon by default. Until
//! then `cargo install --path .` ships an `mp` binary that does not contain a
//! byte of this module.

pub mod lifecycle;
pub mod methods;
pub mod runtime;
pub mod server;
pub mod session;
