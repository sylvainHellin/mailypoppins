//! The one error type every command answers with.
//!
//! Serialised as `{kind, message, ...}` so the frontend branches on `kind`
//! and never on message text. The Rust side has to classify from text in one
//! place, [`GuiError::from_call`], because `mp_client::session` flattens a
//! daemon refusal into a string before it crosses the call channel; the
//! refusal's code survives as the trailing `(<code>)` of
//! `ClientError::Rpc`'s `Display`, which is what [`rpc_code`] reads.

use serde::Serialize;

use mp_protocol::{ErrorCode, PROTOCOL_MAX, PROTOCOL_MIN};

use crate::paths::Paths;

/// The protocol range this build speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct ProtocolRange {
    pub min: u32,
    pub max: u32,
}

impl ProtocolRange {
    pub const fn ours() -> ProtocolRange {
        ProtocolRange {
            min: PROTOCOL_MIN,
            max: PROTOCOL_MAX,
        }
    }
}

/// What a command failed with, tagged by `kind`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GuiError {
    /// No daemon answers (none running, could not be started, or the session
    /// is between reconnect attempts). `socket` and `log` are what the
    /// connection screen prints.
    DaemonUnavailable {
        message: String,
        socket: Option<String>,
        log: Option<String>,
    },
    /// The daemon speaks another protocol or lacks a method this GUI needs;
    /// the frontend shows the blocking restart screen.
    VersionMismatch {
        message: String,
        daemon_version: Option<String>,
        client_protocol: ProtocolRange,
    },
    /// The call went unanswered for its budget.
    Timeout { message: String },
    /// The daemon refused the call or answered something unexpected.
    Protocol { message: String, code: Option<i32> },
    /// The addressed account, message or operation does not exist.
    NotFound { message: String, code: Option<i32> },
    /// The desktop's own setup is wrong: an external editor that would not
    /// start, or a settings file that does not read. `message` names what to
    /// change.
    Setup { message: String },
    /// A fault in this process.
    Internal { message: String },
}

impl std::fmt::Display for GuiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for GuiError {}

/// How a call addresses what it names, which decides what `-32602` means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Addressing {
    /// The call names a row, a message or an operation: `-32602` is "no such
    /// thing" (the daemon's answer for a `row_id` no message has).
    Resource,
    /// The call carries user input (a query): `-32602` is a bad parameter.
    Params,
}

impl GuiError {
    pub fn message(&self) -> &str {
        match self {
            GuiError::DaemonUnavailable { message, .. }
            | GuiError::VersionMismatch { message, .. }
            | GuiError::Timeout { message }
            | GuiError::Protocol { message, .. }
            | GuiError::NotFound { message, .. }
            | GuiError::Setup { message }
            | GuiError::Internal { message } => message,
        }
    }

    pub fn internal(message: impl std::fmt::Display) -> GuiError {
        GuiError::Internal {
            message: message.to_string(),
        }
    }

    pub fn protocol(message: impl std::fmt::Display) -> GuiError {
        GuiError::Protocol {
            message: message.to_string(),
            code: None,
        }
    }

    pub fn not_found(message: impl std::fmt::Display) -> GuiError {
        GuiError::NotFound {
            message: message.to_string(),
            code: None,
        }
    }

    pub fn unavailable(message: impl std::fmt::Display) -> GuiError {
        let paths = Paths::resolve();
        GuiError::DaemonUnavailable {
            message: message.to_string(),
            socket: Some(paths.socket.display().to_string()),
            log: Some(paths.daemon_log.display().to_string()),
        }
    }

    /// Classify the error a session call (or the fixture door) answered.
    pub fn from_call(error: &anyhow::Error, addressing: Addressing) -> GuiError {
        GuiError::from_call_text(&format!("{error:#}"), addressing)
    }

    /// [`GuiError::from_call`] over the text, which is what the session hands
    /// over.
    pub fn from_call_text(text: &str, addressing: Addressing) -> GuiError {
        let message = text.to_string();
        if let Some(code) = rpc_code(text) {
            return match ErrorCode::from_code(code) {
                Some(ErrorCode::AccountUnknown) => GuiError::NotFound {
                    message,
                    code: Some(code),
                },
                Some(ErrorCode::ProtocolIncompatible) | Some(ErrorCode::CapabilityMissing) => {
                    GuiError::VersionMismatch {
                        message,
                        daemon_version: None,
                        client_protocol: ProtocolRange::ours(),
                    }
                }
                Some(ErrorCode::ShuttingDown) => GuiError::unavailable(message),
                _ if code == -32602 && addressing == Addressing::Resource => GuiError::NotFound {
                    message,
                    code: Some(code),
                },
                // Method not found: this daemon does not serve something the
                // GUI calls, which only an older daemon does.
                _ if code == -32601 => GuiError::VersionMismatch {
                    message,
                    daemon_version: None,
                    client_protocol: ProtocolRange::ours(),
                },
                _ => GuiError::Protocol {
                    message,
                    code: Some(code),
                },
            };
        }
        if text.contains("no answer from the daemon") || text.contains("went unanswered") {
            return GuiError::Timeout { message };
        }
        if text.contains("the daemon is not reachable")
            || text.contains("the daemon session is closed")
            || text.contains("the daemon closed the connection")
            || text.contains("daemon socket I/O failed")
            || text.contains("no daemon is listening")
        {
            return GuiError::unavailable(message);
        }
        if text.contains("broke the protocol") {
            return GuiError::protocol(message);
        }
        GuiError::Internal { message }
    }
}

/// The JSON-RPC code of a daemon refusal, from the text
/// `ClientError::Rpc` displays as: `the daemon refused the call: <msg> (<code>)`.
pub fn rpc_code(text: &str) -> Option<i32> {
    let start = text.find("the daemon refused the call")?;
    let tail = &text[start..];
    let open = tail.rfind('(')?;
    let close = tail[open..].find(')')? + open;
    tail[open + 1..close].trim().parse().ok()
}

/// The daemon's own sentence of a refusal, without the method, the
/// `the daemon refused the call:` frame and the trailing `(<code>)`: what a
/// surface shows verbatim (the Graph refusal, a `send.invite` refusal).
pub fn refusal_sentence(text: &str) -> Option<&str> {
    const FRAME: &str = "the daemon refused the call:";
    let start = text.find(FRAME)? + FRAME.len();
    let tail = text[start..].trim();
    let body = match tail.rfind(" (") {
        Some(open)
            if tail.ends_with(')') && tail[open + 2..tail.len() - 1].parse::<i32>().is_ok() =>
        {
            &tail[..open]
        }
        _ => tail,
    };
    Some(body.trim()).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_code_is_read_off_the_refusal_text() {
        let text = "message.html: the daemon refused the call: no markup (x) (-32602)";
        assert_eq!(rpc_code(text), Some(-32602));
        assert_eq!(rpc_code("message.html: the daemon is not reachable"), None);
    }

    #[test]
    fn the_sentence_is_read_off_the_refusal_text() {
        let text = "calendar.rsvp: the daemon refused the call: RSVP is not supported for Graph accounts yet (#0036, blocked on #0035) (-32602)";
        assert_eq!(
            refusal_sentence(text),
            Some("RSVP is not supported for Graph accounts yet (#0036, blocked on #0035)")
        );
        assert_eq!(refusal_sentence("x: the daemon is not reachable"), None);
    }

    #[test]
    fn an_unknown_row_is_not_found_but_a_bad_query_is_protocol() {
        let text = "message.get: the daemon refused the call: no such row (-32602)";
        assert!(matches!(
            GuiError::from_call_text(text, Addressing::Resource),
            GuiError::NotFound {
                code: Some(-32602),
                ..
            }
        ));
        assert!(matches!(
            GuiError::from_call_text(text, Addressing::Params),
            GuiError::Protocol {
                code: Some(-32602),
                ..
            }
        ));
    }

    #[test]
    fn transport_failures_are_classified() {
        assert!(matches!(
            GuiError::from_call_text("x: the daemon is not reachable", Addressing::Params),
            GuiError::DaemonUnavailable { .. }
        ));
        assert!(matches!(
            GuiError::from_call_text(
                "x: no answer from the daemon (timed out waiting on channel)",
                Addressing::Params
            ),
            GuiError::Timeout { .. }
        ));
        assert!(matches!(
            GuiError::from_call_text(
                "x: the daemon refused the call: missing (-32003)",
                Addressing::Params
            ),
            GuiError::VersionMismatch { .. }
        ));
    }

    #[test]
    fn the_error_serialises_with_a_kind_tag() {
        let value = serde_json::to_value(GuiError::Timeout {
            message: "slow".into(),
        })
        .unwrap_or_default();
        assert_eq!(value["kind"], "timeout");
        assert_eq!(value["message"], "slow");
    }
}
