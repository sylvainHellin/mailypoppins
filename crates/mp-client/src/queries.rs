//! The blocking door every client's reads and writes go through (P5-U4,
//! #0124), lifted from `clients/tui` so a second client shares it.
//!
//! Only the trait moved: the typed reads over it (`list_emails`,
//! `mailbox_counts`, `message_body`, ...) decode into the TUI's own model and
//! stay in `mp_tui::queries`, which re-exports [`Queries`] under its old path.

use anyhow::Result;
use serde_json::Value;

/// Something that answers a daemon method call and blocks for the answer.
///
/// One method, with exactly the signature
/// [`Session::call`](crate::session::Session::call) already had, so the
/// implementation for a session is a delegation and no second connect path
/// exists. Object safe on purpose: a client's typed reads take `&dyn Queries`
/// so a test can drive them over a dispatcher in the same process.
pub trait Queries {
    /// Call `method` with `params` and hand back its `result`.
    fn call(&self, method: &str, params: Value) -> Result<Value>;
}

impl Queries for crate::session::Session {
    fn call(&self, method: &str, params: Value) -> Result<Value> {
        crate::session::Session::call(self, method, params)
    }
}

impl Queries for crate::session::QueryHandle {
    fn call(&self, method: &str, params: Value) -> Result<Value> {
        crate::session::QueryHandle::call(self, method, params)
    }
}
