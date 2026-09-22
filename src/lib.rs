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
/// The terminal client, a crate of its own since #0126 (P5-U10f) and
/// re-exported here under the path it had as a module, so `crate::tui::…` and
/// `mailypoppins::tui::…` resolve unchanged in the binary and in the tests.
pub use mp_tui as tui;
pub mod daemon;

/// The TUI tests that are the root crate's own: the sessionless store-backed
/// oracles and every test that compares a served answer against one (#0126,
/// P5-U10e).
///
/// They live here rather than under `src/tui/` because each of them reaches
/// something `crates/mp-tui` may not link - the store, the ingest path, the
/// daemon's own dispatcher - and the crate move is what they are clearing the
/// way for.
#[cfg(test)]
mod tui_tests;
