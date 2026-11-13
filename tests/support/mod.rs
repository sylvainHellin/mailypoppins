//! Shared support code for the root integration tests.
//!
//! This is a **module directory**, not a test target. Cargo autodiscovers
//! `tests/*.rs` and `tests/*/main.rs`; `tests/support/` has neither, so nothing
//! here is compiled unless a test binary writes `mod support;`. Do not add a
//! `tests/support.rs` or a `tests/support/main.rs`: either one turns this into
//! a test binary of its own with no `#[test]` in it.
//!
//! [`parity`] is the Phase 4 parity harness (plan unit P4-U1).
//! [`read_fixture`] is the seeded store the read slice is measured against
//! (plan unit P4-U3).
//! [`draft_fixture`] is that store plus a seeded drafts directory, for the
//! draft slice (plan unit P4-U5).
//! [`mutation_fixture`] is that root plus an ambiguous message, a message
//! whose two attachments share a name and a draft id two accounts hold, for
//! the message-mutation slice (plan unit P4-U7).
//! [`sync_fixture`] is the read fixture's accounts plus one that configures a
//! server it has no credentials for, for the sync/watch slice (plan unit
//! P4-U9).
//! [`send_fixture`] is the draft fixture plus a Graph account, an
//! SMTP-configured account, a seeded outbox and the fake transport, for the
//! send slice (plan unit P4-U11).
//! [`admin_fixture`] is the send fixture plus two invitations and a reply, a
//! file-era mailstore, an id-less draft, a secrets file, a token cache and a
//! prebuilt contact index, for the admin slice (plan unit P4-U13).
//! [`calendar_fixture`] is two accounts, one served by the calendar fake and
//! one without a calendar backend, and an invitation mail on each, for the
//! server calendar (PERSO-200).
//! [`fake_imap`] is a STARTTLS IMAP server whose answer to LOGIN a test
//! switches (refused, temporarily unavailable, or accepted into an empty
//! `INBOX`), for the rejected-login rows (PERSO-296); the library's unit
//! tests include it by path.
//! [`fake_smtp`] is a STARTTLS SMTP server that accepts every login and
//! records the credentials it carried, for the secrets smoke test
//! (PERSO-199).
//! [`readiness`] waits for a spawned daemon to answer `daemon.status`, and
//! [`events`] reads `state.event` notifications past the daemon's own startup
//! events; daemon suites that do not declare `mod support;` include either
//! file by path.
//! [`gate`] holds hooked futures (a tick body, a drain) until a test opens
//! it for a counted number of passes (PERSO-222); the runtime suite and the
//! library's unit tests include it by path.

#![allow(dead_code)]

pub mod admin_fixture;
pub mod calendar_fixture;
pub mod draft_fixture;
pub mod events;
pub mod fake_imap;
pub mod fake_smtp;
pub mod gate;
pub mod mutation_fixture;
pub mod parity;
pub mod read_fixture;
pub mod readiness;
pub mod send_fixture;
pub mod sync_fixture;
