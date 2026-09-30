//! One health check of the `diagnostic.*` family, typed.
//!
//! The same three keys travel three ways: as one `checks` item of a
//! `diagnostic.health` answer, as one entry of the bootstrap snapshot's
//! `diagnostics` array, and as the payload of a `diagnostic.check_changed`
//! event. A client decodes all three with [`HealthCheck`].
//!
//! Not [`crate::events::Diagnostic`], which is `{line, message}`: that one is
//! a position in a draft file that would not parse, this one is a verdict about
//! the daemon itself.

use serde::{Deserialize, Serialize};

/// A check's verdict.
///
/// A closed set, like every enum of this protocol: a fourth word is a daemon
/// the client does not understand, and decoding it as one of the three would be
/// a guess.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// Nothing to do.
    Ok,
    /// Something a user would want to know about and can live with.
    Warn,
    /// Something the daemon cannot do its job through.
    Fail,
}

/// One health check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HealthCheck {
    /// `config_loaded`, `store_open`, `socket_owner`, `log_writable` or
    /// `account:<name>`.
    pub name: String,
    /// The verdict.
    pub status: CheckStatus,
    /// One sentence, never empty.
    pub detail: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    /// Every `checks` item of the committed `diagnostic.health` fixture
    /// decodes and re-encodes to the same JSON.
    #[test]
    fn the_health_fixtures_checks_round_trip() {
        let raw = include_str!("../fixtures/diagnostic.health.response.json");
        let response: Value = serde_json::from_str(raw).expect("the fixture is JSON");
        let checks = response["result"]["checks"].clone();
        let typed: Vec<HealthCheck> =
            serde_json::from_value(checks.clone()).expect("the checks decode");
        assert_eq!(typed.len(), 6);
        assert_eq!(typed[5].status, CheckStatus::Warn);
        assert_eq!(serde_json::to_value(&typed).expect("it serialises"), checks);
    }

    /// A status outside the three is a decode error.
    #[test]
    fn an_unknown_status_is_refused() {
        let decoded: Result<HealthCheck, _> =
            serde_json::from_value(json!({"name": "x", "status": "info", "detail": "y"}));
        assert!(decoded.is_err(), "the status set is closed");
    }
}
