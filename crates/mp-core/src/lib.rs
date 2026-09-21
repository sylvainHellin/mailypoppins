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
//! transport. Three modules arrived split: [`selector`] kept the grammar and
//! left its two store-backed resolvers in the root crate, [`search`] moved
//! whole, taking with it the three pure IMAP string helpers it read out of
//! `imap_client` ([`imap_query`]), and [`invite`] kept the ICS and RSVP
//! building and left `plan_invite`, which reads an account, behind.

pub mod app_state;
pub mod calendar;
pub mod config;
pub mod imap_query;
pub mod invite;
pub mod notify;
pub mod oauth2;
pub mod contacts;
pub mod parse;
pub mod reconcile;
pub mod search;
pub mod secrets;
pub mod selector;
pub mod signatures;
pub mod sync_health;
pub mod timing;
pub mod types;
