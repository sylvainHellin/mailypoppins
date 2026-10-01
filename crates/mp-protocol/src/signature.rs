//! The result of `signature.list` (#0131).
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
