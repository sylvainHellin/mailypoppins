//! A handshake the daemon refused because the two builds do not fit
//! (PERSO-277).
//!
//! `protocol_incompatible` and `capability_missing` end a connect before the
//! `initialize` result that would name the daemon's version, so the refusal's
//! `data` carries `daemon: {version, min, max}` instead. [`VersionRefusal`]
//! reads it once for every client, so `mp`, the TUI and the desktop app name
//! the same versions in the same sentence. A daemon of 0.12.0 or older sends
//! no `version`, and no `daemon` at all in `capability_missing`; the sentence
//! then says so rather than printing "unknown".
//!
//! The way out depends on which side is older. A daemon whose lowest
//! protocol is above this build's highest is the newer one, and restarting
//! it from this build would put it back on the older build, so the sentence
//! says to update this client; otherwise it says to restart the daemon.

use mp_protocol::{ErrorCode, RpcError, PROTOCOL_MAX, PROTOCOL_MIN};
use serde_json::Value;

use crate::{ClientError, ClientKind};

/// Why the builds do not fit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefusalReason {
    /// No protocol version is in both ranges.
    Protocol,
    /// The daemon offers none of these required capabilities.
    Capabilities(Vec<String>),
}

/// A `protocol_incompatible` or `capability_missing` refusal, as a client
/// tells its user about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionRefusal {
    /// Why the handshake was refused.
    pub reason: RefusalReason,
    /// The daemon's application version, when it reports one.
    pub daemon_version: Option<String>,
    /// The daemon's protocol range `(min, max)`, when it reports one.
    pub daemon_protocol: Option<(u32, u32)>,
}

impl VersionRefusal {
    /// The refusal in a client error, or `None` for any other failure.
    pub fn from_error(error: &ClientError) -> Option<VersionRefusal> {
        match error {
            ClientError::Rpc(rpc) => VersionRefusal::from_rpc(rpc),
            _ => None,
        }
    }

    /// The refusal in a JSON-RPC error, or `None` for any other code.
    pub fn from_rpc(error: &RpcError) -> Option<VersionRefusal> {
        let data = error.data.as_ref();
        let reason = match ErrorCode::from_code(error.code)? {
            ErrorCode::ProtocolIncompatible => RefusalReason::Protocol,
            ErrorCode::CapabilityMissing => RefusalReason::Capabilities(
                data.and_then(|d| d.get("missing"))
                    .and_then(Value::as_array)
                    .map(|names| {
                        names
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
            ),
            _ => return None,
        };
        let daemon = data.and_then(|d| d.get("daemon"));
        let number = |key: &str| {
            daemon
                .and_then(|d| d.get(key))
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
        };
        Some(VersionRefusal {
            reason,
            daemon_version: daemon
                .and_then(|d| d.get("version"))
                .and_then(Value::as_str)
                .map(str::to_string),
            daemon_protocol: number("min").zip(number("max")),
        })
    }

    /// Whether this client is the older side: the daemon's lowest protocol
    /// is above the highest this build speaks. A missing capability always
    /// means an older daemon, and a daemon that names no range keeps the
    /// restart advice, since nothing says this client is the older side.
    pub fn client_is_older(&self) -> bool {
        matches!(self.reason, RefusalReason::Protocol)
            && self
                .daemon_protocol
                .is_some_and(|(min, _)| min > PROTOCOL_MAX)
    }

    /// [`describe`](Self::describe) followed by the way out a client of
    /// `kind` offers: update this client when it is the older side, else
    /// restart the daemon on this build.
    pub fn sentence(&self, client_version: &str, kind: ClientKind) -> String {
        format!("{}; {}", self.describe(client_version), self.way_out(kind))
    }

    /// What the user of a client of `kind` does about the refusal.
    fn way_out(&self, kind: ClientKind) -> String {
        let matching = |what: &str| match &self.daemon_version {
            Some(version) => format!("the matching {what} (mailypoppins {version})"),
            None => format!("the {what} of the daemon's version"),
        };
        match (kind, self.client_is_older()) {
            (ClientKind::Cli | ClientKind::Tui, true) => format!(
                "install {} and run it again, since restarting the daemon from this build \
                 would put it back on this older one",
                matching("mp")
            ),
            (ClientKind::Cli | ClientKind::Tui, false) => {
                "restart it on this build with `mp daemon restart`".to_string()
            }
            (ClientKind::Gui, true) => format!(
                "update this app to {}, since restarting the daemon from it would put the \
                 daemon back on this older build",
                matching("version")
            ),
            (ClientKind::Gui, false) => {
                "restart the daemon to run the matching version".to_string()
            }
        }
    }

    /// One sentence naming the daemon's version and protocol, this client's
    /// (`client_version`, and the range this build speaks), and what does not
    /// fit; [`sentence`](Self::sentence) appends the way out.
    pub fn describe(&self, client_version: &str) -> String {
        let daemon = match (&self.daemon_version, self.daemon_protocol) {
            (Some(version), Some(range)) => {
                format!("mailypoppins {version} ({})", protocol(range))
            }
            (Some(version), None) => format!("mailypoppins {version}"),
            (None, Some(range)) => format!(
                "an older mailypoppins that does not report its version ({})",
                protocol(range)
            ),
            (None, None) => "an older mailypoppins that does not report its version".to_string(),
        };
        let client = format!(
            "this client (mailypoppins {client_version}, {})",
            protocol((PROTOCOL_MIN, PROTOCOL_MAX))
        );
        match &self.reason {
            RefusalReason::Protocol => format!(
                "the running daemon is {daemon} and shares no protocol version with {client}"
            ),
            RefusalReason::Capabilities(missing) if missing.is_empty() => {
                format!("the running daemon is {daemon} and lacks capabilities {client} requires")
            }
            RefusalReason::Capabilities(missing) => format!(
                "the running daemon is {daemon} and lacks {}, which {client} requires",
                missing.join(", ")
            ),
        }
    }
}

/// `protocol 1` or `protocol 1 to 2`.
fn protocol((min, max): (u32, u32)) -> String {
    if min == max {
        format!("protocol {min}")
    } else {
        format!("protocol {min} to {max}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rpc(code: ErrorCode, data: Value) -> RpcError {
        RpcError {
            code: code.code(),
            message: code.name().to_string(),
            data: Some(data),
        }
    }

    #[test]
    fn a_capability_refusal_names_both_versions_and_what_is_missing() {
        let refusal = VersionRefusal::from_rpc(&rpc(
            ErrorCode::CapabilityMissing,
            json!({"missing": ["a.b", "c.d"], "daemon": {"version": "0.11.0", "min": 1, "max": 1}}),
        ))
        .expect("a version refusal");
        assert_eq!(refusal.daemon_version.as_deref(), Some("0.11.0"));
        assert_eq!(refusal.daemon_protocol, Some((1, 1)));
        let why = refusal.describe("0.12.0");
        assert!(
            why.starts_with("the running daemon is mailypoppins 0.11.0 (protocol 1)"),
            "{why}"
        );
        assert!(why.contains("lacks a.b, c.d"), "{why}");
        assert!(
            why.contains("this client (mailypoppins 0.12.0, protocol"),
            "{why}"
        );
    }

    #[test]
    fn a_daemon_that_predates_the_version_says_so() {
        let refusal = VersionRefusal::from_rpc(&rpc(
            ErrorCode::ProtocolIncompatible,
            json!({"daemon": {"min": 2, "max": 3}, "client": {"min": 1, "max": 1}}),
        ))
        .expect("a version refusal");
        assert_eq!(refusal.daemon_version, None);
        let why = refusal.describe("0.12.0");
        assert!(
            why.contains("does not report its version (protocol 2 to 3)"),
            "{why}"
        );
        assert!(why.contains("shares no protocol version"), "{why}");

        let bare = VersionRefusal::from_rpc(&RpcError {
            code: ErrorCode::CapabilityMissing.code(),
            message: "capability_missing".into(),
            data: None,
        })
        .expect("a version refusal without data");
        assert!(bare.describe("0.12.0").contains("lacks capabilities"));
    }

    /// A daemon whose lowest protocol is above this build's highest is the
    /// newer side, and restarting it from this build would put it back on the
    /// older one: the way out is to update this client. A daemon below this
    /// build's range, one that does not name its range, and one that lacks a
    /// capability are the older side, which a restart replaces.
    #[test]
    fn the_way_out_updates_whichever_side_is_older() {
        let newer = VersionRefusal::from_rpc(&rpc(
            ErrorCode::ProtocolIncompatible,
            json!({"daemon": {"version": "9.0.0", "min": PROTOCOL_MAX + 1, "max": PROTOCOL_MAX + 1}}),
        ))
        .expect("a version refusal");
        assert!(newer.client_is_older());
        let cli = newer.sentence("0.12.0", ClientKind::Cli);
        assert!(cli.starts_with(&newer.describe("0.12.0")), "{cli}");
        assert!(
            cli.ends_with(
                "; install the matching mp (mailypoppins 9.0.0) and run it again, since restarting \
                 the daemon from this build would put it back on this older one"
            ),
            "{cli}"
        );
        assert_eq!(cli, newer.sentence("0.12.0", ClientKind::Tui));
        let gui = newer.sentence("0.12.0", ClientKind::Gui);
        assert!(
            gui.ends_with(
                "; update this app to the matching version (mailypoppins 9.0.0), since restarting \
                 the daemon from it would put the daemon back on this older build"
            ),
            "{gui}"
        );

        let older = VersionRefusal::from_rpc(&rpc(
            ErrorCode::ProtocolIncompatible,
            json!({"daemon": {"version": "0.0.1", "min": 0, "max": PROTOCOL_MIN - 1}}),
        ))
        .expect("a version refusal");
        let unranged = VersionRefusal::from_rpc(&rpc(ErrorCode::ProtocolIncompatible, json!({})))
            .expect("a version refusal without a range");
        let lacking = VersionRefusal::from_rpc(&rpc(
            ErrorCode::CapabilityMissing,
            json!({"missing": ["a.b"], "daemon": {"version": "9.0.0", "min": PROTOCOL_MAX + 1, "max": PROTOCOL_MAX + 1}}),
        ))
        .expect("a version refusal");
        for refusal in [older, unranged, lacking] {
            assert!(!refusal.client_is_older(), "{refusal:?}");
            assert!(
                refusal
                    .sentence("0.12.0", ClientKind::Cli)
                    .ends_with("; restart it on this build with `mp daemon restart`"),
                "{refusal:?}"
            );
            assert!(
                refusal
                    .sentence("0.12.0", ClientKind::Gui)
                    .ends_with("; restart the daemon to run the matching version"),
                "{refusal:?}"
            );
        }
    }

    #[test]
    fn other_refusals_are_not_version_refusals() {
        assert!(VersionRefusal::from_rpc(&rpc(ErrorCode::IdentityMismatch, json!({}))).is_none());
        assert!(VersionRefusal::from_error(&ClientError::NotRunning).is_none());
    }
}
