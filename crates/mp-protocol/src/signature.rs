//! The results of the `signature.*` family: `signature.list` (#0131), and the
//! file `signature.read` and `signature.create` answer (the daemon leftovers,
//! 2026-10-22).
//!
//! A signature is a file under the config directory's `signatures/`, shared
//! by every account; which one an account starts with is per account. A
//! client offers the names in a compose wizard with the default preselected,
//! and used to read the directory itself to do it.

use serde::{Deserialize, Serialize};

/// The `result` of `signature.list`: the signatures a draft of `account` can
/// carry, and the account's default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SignatureListing {
    /// The account the default is about.
    pub account: String,
    /// Every signature name, sorted; a file whose stem is not a valid name is
    /// left out, since no other method would take it.
    pub names: Vec<String>,
    /// The account's default, `null` when it has none or its file is gone.
    pub default: Option<String>,
}

/// One signature: its name, its file and what the file holds, as
/// `signature.read` and `signature.create` answer it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SignatureFile {
    /// The file stem, which is the name a draft writer takes.
    pub name: String,
    /// The absolute path of the file, which is what an editor opens; no
    /// method writes a signature's content.
    pub path: String,
    /// The raw Markdown, empty for a signature just created.
    pub content: String,
}
