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
//! transport. Six modules arrived split. P5-U10a brought three: [`selector`]
//! kept the grammar and left its two store-backed resolvers in the root crate,
//! [`search`] moved whole, taking with it the three pure IMAP string helpers it
//! read out of `imap_client` ([`imap_query`]), and [`invite`] kept the ICS and
//! RSVP building and left `plan_invite`, which reads an account, behind.
//! P5-U10b brought the other three: [`reconcile`] kept the fold and left the
//! three readers that open a store, [`contacts`] kept everything but the store
//! rebuild and the sync/send hooks, and [`draft`] kept the file format and left
//! the four operations that need an index, a row or an outbox record.
//! [`addresses`] arrived the way [`imap_query`] did, four pure string functions
//! out of `send` that `draft` reads and that want no transport.

pub mod addresses;
pub mod app_state;
pub mod calendar;
pub mod config;
pub mod imap_query;
pub mod invite;
pub mod notify;
pub mod oauth2;
pub mod contacts;
pub mod draft;
pub mod parse;
pub mod reconcile;
pub mod search;
pub mod secrets;
pub mod selector;
pub mod signatures;
pub mod sync_health;
pub mod timing;
pub mod types;
