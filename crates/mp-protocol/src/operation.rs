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

use crate::RpcError;

/// The five states of an operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Whether nothing can move this operation any further.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            OperationState::Succeeded | OperationState::Failed | OperationState::Cancelled
        )
    }
}

/// What a disconnect of the starting connection does to an operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelScope {
    /// The work outlives the connection that asked for it.
    Durable,
    /// The work is cancelled when its connection closes.
    ClientScoped,
}

/// One progress report: the `progress` member of a status and the body of an
/// `operation.progress` event.
///
/// `total` and `message` travel as `null` rather than as absent keys, because a
/// client reads `total` to draw a bar and has to tell "unknown" from "missing".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    /// What the operation is doing now.
    pub phase: String,
    /// How much of it is done.
    pub done: u64,
    /// How much there is, `null` when the total is not known yet.
    pub total: Option<u64>,
    /// One line for the user, when there is one.
    pub message: Option<String>,
}

/// Everything the daemon says about one operation.
///
/// Every member is always present: `progress`, `result` and `error` are `null`
/// when there is nothing to say, never absent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    pub result: Option<Value>,
    /// Why a failed or cancelled operation stopped, the JSON-RPC error object
    /// the method would have answered with.
    pub error: Option<RpcError>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    /// A state outside the five is a decode error rather than a silent guess.
    #[test]
    fn an_unknown_state_is_refused() {
        let decoded: Result<OperationState, _> = serde_json::from_value(json!("paused"));
        assert!(decoded.is_err(), "the state set is closed");
    }
}
