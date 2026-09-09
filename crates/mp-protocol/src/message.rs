//! The five message shapes on the wire, plus the event envelope.
//!
//! Serialisation rules the fixtures depend on: an absent `id` and an absent
//! `data` are omitted rather than written as `null`, and [`RequestId`] is
//! untagged, so a number stays a number and a string stays a string.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A JSON-RPC request id, either a number or a string, echoed back unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    /// A numeric id, which is what every mailypoppins client sends.
    Num(i64),
    /// A string id, which JSON-RPC allows and the daemon must echo verbatim.
    Str(String),
}

/// A call from a client to the daemon.
///
/// `id` is absent for a notification-style request, which expects no response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// The correlation id, omitted when the client wants no answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<RequestId>,
    /// `initialize` or a `family.name` domain method.
    pub method: String,
    /// Named parameters, always an object in this protocol, never positional.
    #[serde(default)]
    pub params: Value,
}

/// A successful answer to a [`Request`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// The id of the request being answered.
    pub id: RequestId,
    /// The method's result, shaped by the method.
    pub result: Value,
}

/// A failed answer to a [`Request`].
///
/// `id` is absent when the failure happened before an id could be read, which
/// is the JSON-RPC parse-error case.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ErrorResponse {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// The id of the failed request, absent when it could not be parsed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<RequestId>,
    /// The failure itself.
    pub error: RpcError,
}

/// The `error` member of an [`ErrorResponse`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    /// A JSON-RPC standard code or a [`crate::ErrorCode`] value.
    pub code: i32,
    /// A human-readable one-line summary for logs and for the CLI.
    pub message: String,
    /// The structured payload the error table fixes for this code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// A message that expects no answer, in either direction.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// `state.event`, `state.resync_required`, or a later control method.
    pub method: String,
    /// Named parameters, always an object.
    #[serde(default)]
    pub params: Value,
}

/// A state change the daemon broadcasts, carried as the params of a
/// `state.event` [`Notification`].
///
/// This is not a JSON-RPC message on its own, so it declares no `jsonrpc`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// The daemon instance that produced the event.
    pub instance_id: String,
    /// The monotonic state revision the event moves the client to; `0` is the
    /// pre-bootstrap sentinel and never appears on the wire.
    pub revision: u64,
    /// Selects the client's handler, for example `message.flags_changed`.
    pub kind: String,
    /// The kind's payload, always an object so it can gain fields.
    pub payload: Value,
}
