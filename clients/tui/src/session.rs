//! The TUI's daemon session, which lives in `mp_client::session` since the
//! client kernel moved there so every client shares it; re-exported so
//! `mp_tui::session::…` (and `mailypoppins::tui::session::…`) resolve where they
//! always did.

pub use mp_client::session::{Connector, OpenSession, QueryHandle, ReopenSession, Session};
