//! The engine-free half of mailypoppins: everything a client needs and no part
//! of the engine (#0126, unit P5-U10a).
//!
//! The daemon migration's last phase moves the TUI into its own crate, and the
//! obstacle was never the engine residue the allow-list records: it was the
//! *shared* modules, which both the client and the engine read. This crate is
//! that shared set, moved out of the root crate whole and re-exported from it
//! under every old path, so no call site outside these files changed.
//!
//! What lives here reaches no store, no IMAP session, no outbox and no sending
//! transport. [`selector`] arrived split: the grammar is here, the two
//! store-backed resolvers stayed in the root crate.

pub mod app_state;
pub mod calendar;
pub mod config;
pub mod notify;
pub mod oauth2;
pub mod parse;
pub mod secrets;
pub mod selector;
pub mod signatures;
pub mod sync_health;
pub mod timing;
pub mod types;
