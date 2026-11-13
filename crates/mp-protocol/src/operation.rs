//! The `operation.status` result, typed.
//!
//! One shape travels four ways: as the `operation.status` result, as one entry
//! of the bootstrap snapshot's `operations` array, as one entry of the
//! `pending` a `daemon.stop` answers and a `daemon.shutting_down` event
//! carries, and as one entry of the `unsettled` a `daemon.stopped` closes with.
//! The daemon renders every one of them from its registry through
//! [`OperationStatus`], so the four cannot drift apart, and a client decodes
//! all four with it.
//!
//! `state` and `scope` are enums rather than strings for the reason
//! [`crate::state`] gives: they are closed sets the protocol version fixes, and
//! a client branches on them. The connection that started an operation is not
//! on the wire: it is an id inside the daemon that no client can address.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::send::SendOutcome;
use crate::RpcError;

/// The five states of an operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    /// Accepted, not started.
    Queued,
    /// Working.
    Running,
    /// Finished with a result.
    Succeeded,
    /// Finished with an error.
    Failed,
    /// Stopped, by a client or by its owner's disconnect.
    Cancelled,
}

impl OperationState {
    /// The string this state travels as, serde's spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            OperationState::Queued => "queued",
            OperationState::Running => "running",
            OperationState::Succeeded => "succeeded",
            OperationState::Failed => "failed",
            OperationState::Cancelled => "cancelled",
        }
    }

    /// The state a wire string names, or `None` for anything else.
    pub fn from_wire(value: &str) -> Option<OperationState> {
        match value {
            "queued" => Some(OperationState::Queued),
            "running" => Some(OperationState::Running),
            "succeeded" => Some(OperationState::Succeeded),
            "failed" => Some(OperationState::Failed),
            "cancelled" => Some(OperationState::Cancelled),
            _ => None,
        }
    }

    /// Whether nothing can move this operation any further.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            OperationState::Succeeded | OperationState::Failed | OperationState::Cancelled
        )
    }
}

/// What a disconnect of the starting connection does to an operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CancelScope {
    /// The work outlives the connection that asked for it.
    Durable,
    /// The work is cancelled when its connection closes.
    ClientScoped,
}

impl CancelScope {
    /// The string this scope travels as, serde's spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            CancelScope::Durable => "durable",
            CancelScope::ClientScoped => "client_scoped",
        }
    }

    /// The scope a wire string names, or `None` for anything else.
    pub fn from_wire(value: &str) -> Option<CancelScope> {
        match value {
            "durable" => Some(CancelScope::Durable),
            "client_scoped" => Some(CancelScope::ClientScoped),
            _ => None,
        }
    }
}

/// One progress report: the `progress` member of a status and the body of an
/// `operation.progress` event.
///
/// `total` and `message` travel as `null` rather than as absent keys, because a
/// client reads `total` to draw a bar and has to tell "unknown" from "missing".
/// `outcome` is the exception: absent unless a report settles one send, so
/// every other operation's report is the four members it always was.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Progress {
    /// What the operation is doing now.
    pub phase: String,
    /// How much of it is done.
    pub done: u64,
    /// How much there is, `null` when the total is not known yet.
    pub total: Option<u64>,
    /// One line for the user, when there is one.
    pub message: Option<String>,
    /// The result of the one send this report settles: `send.approved`'s
    /// `draft_done` report carries the draft's [`SendOutcome`], the element
    /// the batch's answer will hold for it (PERSO-223), so a client renders
    /// the line as the draft settles with the wording it uses for the answer.
    /// Absent on every other report and from a daemon that predates it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub outcome: Option<SendOutcome>,
}

/// Everything the daemon says about one operation.
///
/// Every member is always present: `progress`, `result` and `error` are `null`
/// when there is nothing to say, never absent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct OperationStatus {
    /// The opaque id the starting method answered with.
    pub operation_id: String,
    /// The method that started it.
    pub method: String,
    /// Where it is in the machine.
    pub state: OperationState,
    /// What a disconnect of the starting connection does to it.
    pub scope: CancelScope,
    /// The newest report, kept after the finish so a client that missed it can
    /// still read it.
    pub progress: Option<Progress>,
    /// What a succeeded operation produced.
    #[cfg_attr(feature = "ts", ts(type = "unknown"))]
    pub result: Option<Value>,
    /// Why a failed or cancelled operation stopped, the JSON-RPC error object
    /// the method would have answered with.
    pub error: Option<RpcError>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `send.approved`'s `draft_done` report decodes as a [`Progress`] whose
    /// `outcome` is the draft's [`SendOutcome`] and re-encodes to the same
    /// JSON; a report without one (every other operation's, the committed
    /// `notification.operation_progress.json`) serialises no `outcome` key, so
    /// its bytes are the ones it always had.
    #[test]
    fn a_draft_done_report_carries_the_drafts_outcome_and_others_carry_none() {
        let raw = include_str!("../fixtures/notification.operation_progress_draft_done.json");
        let event: Value = serde_json::from_str(raw).expect("the fixture is JSON");
        let mut payload = event["params"]["payload"].clone();
        payload.as_object_mut().unwrap().remove("operation_id");
        let report: Progress = serde_json::from_value(payload.clone()).expect("it decodes");
        assert_eq!(report.phase, "draft_done");
        assert_eq!(
            report.message, None,
            "the daemon sends the data, not a line"
        );
        let outcome = report.outcome.as_ref().expect("the draft's outcome");
        assert_eq!(outcome.recipients.len(), 2);
        assert_eq!(serde_json::to_value(&report).unwrap(), payload);

        let raw = include_str!("../fixtures/notification.operation_progress.json");
        let event: Value = serde_json::from_str(raw).expect("the fixture is JSON");
        let mut payload = event["params"]["payload"].clone();
        payload.as_object_mut().unwrap().remove("operation_id");
        let report: Progress = serde_json::from_value(payload.clone()).expect("it decodes");
        assert_eq!(report.outcome, None);
        assert_eq!(serde_json::to_value(&report).unwrap(), payload);
    }

    /// The committed `operation.status` fixture decodes into the type and
    /// re-encodes to the same JSON.
    #[test]
    fn the_committed_fixture_round_trips() {
        let raw = include_str!("../fixtures/operation.status.response.json");
        let response: Value = serde_json::from_str(raw).expect("the fixture is JSON");
        let status: OperationStatus =
            serde_json::from_value(response["result"].clone()).expect("the result decodes");
        assert_eq!(status.state, OperationState::Running);
        assert_eq!(status.scope, CancelScope::Durable);
        assert_eq!(
            status.progress.as_ref().map(|progress| progress.total),
            Some(Some(214))
        );
        assert_eq!(
            serde_json::to_value(&status).expect("it serialises"),
            response["result"]
        );
    }

    /// A settled failure carries its error object, with `data` omitted rather
    /// than nulled when it has none, as everywhere else in this protocol.
    #[test]
    fn a_failure_keeps_the_error_object_shape() {
        let status = OperationStatus {
            operation_id: "op-1".to_string(),
            method: "sync.quick".to_string(),
            state: OperationState::Failed,
            scope: CancelScope::ClientScoped,
            progress: None,
            result: None,
            error: Some(RpcError {
                code: -32603,
                message: "boom".to_string(),
                data: None,
            }),
        };
        let encoded = serde_json::to_value(&status).expect("it serialises");
        assert_eq!(encoded["scope"], json!("client_scoped"));
        assert_eq!(encoded["progress"], Value::Null);
        assert_eq!(encoded["error"], json!({"code": -32603, "message": "boom"}));
        let decoded: OperationStatus = serde_json::from_value(encoded).expect("it decodes");
        assert_eq!(decoded, status);
        assert!(decoded.state.is_terminal());
    }

    /// `as_str` and `from_wire` spell every variant exactly as serde does, so
    /// a payload the daemon builds by hand and one it serialises agree, and a
    /// near miss is refused rather than guessed.
    #[test]
    fn the_string_spellings_are_the_serde_ones() {
        for state in [
            OperationState::Queued,
            OperationState::Running,
            OperationState::Succeeded,
            OperationState::Failed,
            OperationState::Cancelled,
        ] {
            assert_eq!(json!(state.as_str()), json!(state));
            assert_eq!(OperationState::from_wire(state.as_str()), Some(state));
        }
        for scope in [CancelScope::Durable, CancelScope::ClientScoped] {
            assert_eq!(json!(scope.as_str()), json!(scope));
            assert_eq!(CancelScope::from_wire(scope.as_str()), Some(scope));
        }
        assert_eq!(OperationState::from_wire("stopped"), None);
        assert_eq!(CancelScope::from_wire("client-scoped"), None);
    }

    /// A state outside the five is a decode error rather than a silent guess.
    #[test]
    fn an_unknown_state_is_refused() {
        let decoded: Result<OperationState, _> = serde_json::from_value(json!("paused"));
        assert!(decoded.is_err(), "the state set is closed");
    }
}
