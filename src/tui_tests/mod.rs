//! The TUI tests the root crate owns (#0126, P5-U10e).
//!
//! `src/tui/` is about to become `crates/mp-tui`, a client crate that links
//! `mp-core`, `mp-client` and `mp-protocol` and nothing else. Most of what the
//! TUI tests is the TUI's own and moves with it; what could not is here, in the
//! crate that owns what each of these reaches:
//!
//! - [`oracle`], the sessionless store-backed readers every daemon-backed
//!   answer is compared against, and the store reads that came with them;
//! - [`daemon`], the in-process dispatcher over a fixture data root that makes
//!   those comparisons real rather than a JSON mock;
//! - [`queries`], [`invites`] and [`types`], the equality suites themselves;
//! - [`actions`], [`actions_store`], [`commands`], [`events`], [`hold`] and
//!   [`preview`], the client tests whose fixtures seed a store through the
//!   ingest path or drive a daemon method end to end;
//! - [`golden_frames_daemon`], the twenty-three frames built from a real
//!   `state.bootstrap`, whose fixtures come from the hand-built suite that
//!   stays in the TUI.
//!
//! Not one of them changed what it asserts on the way here. What changed is
//! which crate compiles them, and the sessionless `App` they used to read a
//! store through: an `App` with no session answers an empty list and no card
//! now, so where a test used to compare two branches of the same method it
//! compares the served answer against [`oracle`] instead.

mod actions;
mod actions_store;
mod commands;
mod daemon;
mod events;
mod golden_frames_daemon;
mod hold;
mod invites;
mod oracle;
mod preview;
mod queries;
mod types;
