// The engine-free shared modules live in `mp-core` since #0126 (P5-U10a) and
// are re-exported here under the paths they had when they were files in this
// crate, so that `crate::config::…` / `mailypoppins::parse::…` keep resolving
// everywhere: the CLI, the daemon, the TUI and the integration tests.
pub use mp_core::{
    app_state, calendar, config, notify, oauth2, parse, search, secrets, signatures, sync_health,
    timing, types,
};

pub mod agenda;
pub mod config_cmd;
pub mod contacts;
pub mod contacts_cmd;
pub mod graph;
pub mod invite;
pub mod reconcile;
pub mod calendar_cmd;
pub mod cutover;
pub mod imap_client;
pub mod ingest;
pub mod draft;
pub mod draft_cmd;
pub mod selector;
pub mod dump;
pub mod read_cmd;
pub mod engine_lock;
pub mod mutations;
pub mod ops;
pub mod outbox;
pub mod pending_ops;
pub mod send;
pub mod store;
pub mod sync;
pub mod tui;
pub mod daemon;
