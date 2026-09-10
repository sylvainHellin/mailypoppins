//! The long-lived local daemon: runtime files, the startup lock, and (from
//! later units) the socket server and its lifecycle commands.
//!
//! The module was gated behind the `daemon` cargo feature for the first three
//! phases of the migration described in
//! `.agents/workflow/native-gui-daemon/plan.md`. P4-U1 removed the gate,
//! because from Phase 4 clients require a daemon by default, so
//! `cargo install --path .` now ships it.
//!
//! [`client`] is the other side of the socket: the policy a normal `mp` run
//! applies before it reaches any of this.

pub mod client;
pub mod config;
pub mod dispatch;
pub mod handles;
pub mod lifecycle;
pub mod methods;
pub mod operations;
pub mod runtime;
pub mod server;
pub mod session;
pub mod state;
pub mod sync_outcome;
pub mod watch;
