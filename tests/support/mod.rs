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

#![allow(dead_code)]

pub mod draft_fixture;
pub mod parity;
pub mod read_fixture;
