//! The result of `account.credentials`: whether each account's secrets are
//! there, never what they are.
//!
//! `config.get` reports `<redacted>` for every password whether one is stored
//! or not, by contract: a redacted read may not go and look a secret up. This
//! is the separate question a settings screen asks, answered by the process
//! that owns the secrets backend and the token caches, with booleans and a
//! token verdict only. No value, length, prefix or key name travels.

use serde::{Deserialize, Serialize};

/// What the OAuth2 token cache of a `graph` account holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum TokenState {
    /// A cached token that has not expired.
    Valid,
    /// A cached token past its expiry; the next use refreshes it or asks for
    /// a new sign-in.
    Expired,
    /// A cache file that is there and does not decrypt or parse.
    Invalid,
    /// No cache file: the account was never signed in on this machine.
    NotCached,
}

/// One account's secrets, as presence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AccountCredential {
    /// The configured account name.
    pub account: String,
    /// `password` or `graph`, the account's `auth_method`; `oauth2` for an
    /// account that still names the retired mode, which reports no secret.
    pub auth_method: String,
    /// Whether `smtp-password-<account>` is stored; `null` for any account
    /// but a password one.
    pub smtp_password: Option<bool>,
    /// Whether `imap-password-<account>` is stored, the IMAP key alone: an
    /// account without one falls back to the SMTP password, which a client
    /// says itself from the two booleans. `null` for any account but a
    /// password one.
    pub imap_password: Option<bool>,
    /// The token cache's verdict for a `graph` account, `null` for any
    /// other.
    pub token: Option<TokenState>,
}

/// The `result` of `account.credentials`: every configured account in the
/// file's order, or the one the call named.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AccountCredentials {
    /// One entry per account.
    pub accounts: Vec<AccountCredential>,
}
