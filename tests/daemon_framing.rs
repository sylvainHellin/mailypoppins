//! Wire types, error codes, and newline framing for the daemon protocol
//! (#0120, unit P2-U2).
//!
//! This file is a **contract test**: it is written before `mp-protocol` has any
//! contents, against the API fixed in
//! `.agents/workflow/native-gui-daemon/plan.md` section 3.3 (unit P2-U2), and
//! it compiles only under `--features daemon` so the pre-daemon tree keeps
//! building. Until P2-U3 lands, `cargo test --workspace --features daemon` must
//! fail with unresolved-import errors naming exactly the items below and
//! nothing else. An implementer does not edit this file; they make it pass.
//!
//! The numeric error codes are pinned here and in the fixtures, and section 3.0
//! says nothing may change them afterwards without a protocol-changelog entry.
//!
//! Framing rule under test: one JSON-RPC message per line, UTF-8, `\n`
//! terminated, no raw newline inside a frame, request frames capped at
//! [`MAX_REQUEST_BYTES`] (1 MiB). The cap is **inclusive of the terminator**:
//! a frame whose encoded length is exactly the limit is accepted, one byte more
//! is [`FrameError::TooLarge`].

use serde_json::{json, Value};

use mp_protocol::frame::{self, Decoder, FrameError};
use mp_protocol::{
    ErrorCode, ErrorResponse, EventEnvelope, Notification, Request, RequestId, Response, RpcError,
    MAX_REQUEST_BYTES, PROTOCOL_MAX, PROTOCOL_MIN,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Phase 2 advertises `{min: 1, max: 1}` (plan section 3.0).
// The range assertion is constant while both ends are 1, which is exactly what
// the two assertions above pin; it stays because it is the one that still holds
// when a later phase raises `PROTOCOL_MAX`.
#[allow(clippy::assertions_on_constants)]
#[test]
fn protocol_version_range_is_one_to_one() {
    assert_eq!(PROTOCOL_MIN, 1, "PROTOCOL_MIN must be 1 in Phase 2");
    assert_eq!(PROTOCOL_MAX, 1, "PROTOCOL_MAX must be 1 in Phase 2");
    assert!(
        PROTOCOL_MIN <= PROTOCOL_MAX,
        "an empty version range would refuse every client"
    );
}

/// The request frame cap is 1 MiB, spelled two ways so a typo in either the
/// shift or the decimal is caught.
#[test]
fn max_request_bytes_is_one_mebibyte() {
    assert_eq!(MAX_REQUEST_BYTES, 1 << 20);
    assert_eq!(MAX_REQUEST_BYTES, 1_048_576);
}

// ---------------------------------------------------------------------------
// Error codes
// ---------------------------------------------------------------------------

/// Every variant, in the order of the table in plan section 3.0, with the exact
/// number and name that table fixes. `ErrorCode::code(self)` and
/// `ErrorCode::name(self)` take `self` by value, so `ErrorCode` must be `Copy`
/// for this array to be usable twice.
const ERROR_CODES: &[(ErrorCode, i32, &str)] = &[
    (ErrorCode::NotInitialized, -32000, "not_initialized"),
    (ErrorCode::IdentityMismatch, -32001, "identity_mismatch"),
    (
        ErrorCode::ProtocolIncompatible,
        -32002,
        "protocol_incompatible",
    ),
    (ErrorCode::CapabilityMissing, -32003, "capability_missing"),
    (ErrorCode::FrameTooLarge, -32004, "frame_too_large"),
    (ErrorCode::AccountUnknown, -32005, "account_unknown"),
    (ErrorCode::AccountNotReady, -32006, "account_not_ready"),
    (ErrorCode::ConfigInvalid, -32007, "config_invalid"),
    (ErrorCode::OperationCancelled, -32008, "operation_cancelled"),
    (ErrorCode::ShuttingDown, -32009, "shutting_down"),
];

#[test]
fn every_error_code_maps_to_its_pinned_number_and_name() {
    for (variant, code, name) in ERROR_CODES {
        assert_eq!(
            variant.code(),
            *code,
            "ErrorCode::{name} must stay at {code}; changing it needs a protocol-changelog entry"
        );
        assert_eq!(
            variant.name(),
            *name,
            "the wire name for code {code} is pinned by the fixtures"
        );
    }
}

#[test]
fn error_codes_and_names_are_unique() {
    let mut codes: Vec<i32> = ERROR_CODES.iter().map(|(v, _, _)| v.code()).collect();
    let mut names: Vec<&str> = ERROR_CODES.iter().map(|(v, _, _)| v.name()).collect();
    codes.sort_unstable();
    names.sort_unstable();
    let unique_codes = {
        let mut c = codes.clone();
        c.dedup();
        c
    };
    let unique_names = {
        let mut n = names.clone();
        n.dedup();
        n
    };
    assert_eq!(codes, unique_codes, "two variants share a numeric code");
    assert_eq!(names, unique_names, "two variants share a wire name");
}

/// The daemon range must not collide with the JSON-RPC standard codes, which
/// the daemon also emits (`-32700` parse error, `-32600` invalid request,
/// `-32601` method not found, `-32602` invalid params, `-32603` internal).
#[test]
fn daemon_codes_avoid_the_json_rpc_standard_range() {
    const STANDARD: [i32; 5] = [-32700, -32600, -32601, -32602, -32603];
    for (variant, _, name) in ERROR_CODES {
        let code = variant.code();
        assert!(
            !STANDARD.contains(&code),
            "{name} ({code}) collides with a standard JSON-RPC code"
        );
        assert!(
            (-32009..=-32000).contains(&code),
            "{name} ({code}) falls outside the daemon range -32009..=-32000"
        );
    }
}

// ---------------------------------------------------------------------------
// Message shapes
// ---------------------------------------------------------------------------

fn sample_request() -> Request {
    Request {
        jsonrpc: "2.0".to_string(),
        id: Some(RequestId::Num(7)),
        method: "state.bootstrap".to_string(),
        params: json!({"since_revision": 41}),
    }
}

fn sample_response() -> Response {
    Response {
        jsonrpc: "2.0".to_string(),
        id: RequestId::Num(7),
        result: json!({"revision": 42, "instance_id": "01J000000000000000000000"}),
    }
}

fn sample_error_response() -> ErrorResponse {
    ErrorResponse {
        jsonrpc: "2.0".to_string(),
        id: Some(RequestId::Str("init-1".to_string())),
        error: RpcError {
            code: ErrorCode::NotInitialized.code(),
            message: "the connection has not completed initialize".to_string(),
            data: Some(json!({})),
        },
    }
}

fn sample_notification() -> Notification {
    Notification {
        jsonrpc: "2.0".to_string(),
        method: "state.resync_required".to_string(),
        params: json!({"instance_id": "01J000000000000000000000", "reason": "event_queue_overflow"}),
    }
}

fn sample_event() -> EventEnvelope {
    EventEnvelope {
        instance_id: "01J000000000000000000000".to_string(),
        revision: 42,
        kind: "message.flags_changed".to_string(),
        payload: json!({"account": "tum", "mailbox": "INBOX", "uids": [1, 2, 3]}),
    }
}

/// `RequestId` is untagged on the wire: a number stays a number, a string stays
/// a string. JSON-RPC allows both and a client that sent one must get the same
/// back.
#[test]
fn request_id_is_untagged_on_the_wire() {
    assert_eq!(serde_json::to_value(RequestId::Num(7)).unwrap(), json!(7));
    assert_eq!(
        serde_json::to_value(RequestId::Str("init-1".to_string())).unwrap(),
        json!("init-1")
    );
    let back: RequestId = serde_json::from_value(json!(-3)).unwrap();
    assert_eq!(serde_json::to_value(back).unwrap(), json!(-3));
    let back: RequestId = serde_json::from_value(json!("abc")).unwrap();
    assert_eq!(serde_json::to_value(back).unwrap(), json!("abc"));
}

/// An absent `id` and an absent `data` are **omitted**, not serialised as
/// `null`. A JSON-RPC notification-style request carries no `id` key at all,
/// and an error whose `data` is `None` carries no `data` key.
#[test]
fn optional_fields_are_omitted_rather_than_null() {
    let request = Request {
        jsonrpc: "2.0".to_string(),
        id: None,
        method: "daemon.ping".to_string(),
        params: json!({}),
    };
    let encoded = serde_json::to_value(&request).unwrap();
    assert_eq!(
        encoded,
        json!({"jsonrpc": "2.0", "method": "daemon.ping", "params": {}}),
        "a request without an id must not emit `\"id\": null`"
    );
    let back: Request = serde_json::from_value(encoded).unwrap();
    assert!(back.id.is_none());

    let error = ErrorResponse {
        jsonrpc: "2.0".to_string(),
        id: None,
        error: RpcError {
            code: -32700,
            message: "parse error".to_string(),
            data: None,
        },
    };
    assert_eq!(
        serde_json::to_value(&error).unwrap(),
        json!({"jsonrpc": "2.0", "error": {"code": -32700, "message": "parse error"}}),
        "absent id and absent data must both be omitted"
    );
}

#[test]
fn event_envelope_carries_the_pinned_field_names() {
    assert_eq!(
        serde_json::to_value(sample_event()).unwrap(),
        json!({
            "instance_id": "01J000000000000000000000",
            "revision": 42,
            "kind": "message.flags_changed",
            "payload": {"account": "tum", "mailbox": "INBOX", "uids": [1, 2, 3]}
        })
    );
}

// ---------------------------------------------------------------------------
// Framing: encode
// ---------------------------------------------------------------------------

/// `frame::encode` appends exactly one `\n` and puts it last, for every message
/// kind. A frame with an interior raw newline would desynchronise the decoder.
#[test]
fn encode_terminates_each_frame_with_a_single_trailing_newline() {
    let frames = [
        frame::encode(&sample_request()).unwrap(),
        frame::encode(&sample_response()).unwrap(),
        frame::encode(&sample_error_response()).unwrap(),
        frame::encode(&sample_notification()).unwrap(),
        frame::encode(&sample_event()).unwrap(),
    ];
    for bytes in &frames {
        assert_eq!(bytes.last(), Some(&b'\n'), "frame must end with a newline");
        assert_eq!(
            bytes.iter().filter(|b| **b == b'\n').count(),
            1,
            "a frame must contain no raw newline other than its terminator"
        );
        std::str::from_utf8(bytes).expect("frames are UTF-8");
    }
}

// ---------------------------------------------------------------------------
// Framing: round trips for all five message kinds
// ---------------------------------------------------------------------------

/// Encode, feed the bytes to a fresh decoder, and require exactly one value
/// back that is equal to the message serialised directly.
fn round_trip<T: serde::Serialize>(msg: &T) -> Value {
    let bytes = frame::encode(msg).expect("encode");
    let mut decoder = Decoder::new(MAX_REQUEST_BYTES);
    let mut values = decoder.push(&bytes).expect("push");
    assert_eq!(values.len(), 1, "one frame in, one value out");
    values.remove(0)
}

#[test]
fn request_round_trips_through_a_frame() {
    let msg = sample_request();
    let value = round_trip(&msg);
    assert_eq!(value, serde_json::to_value(&msg).unwrap());
    let back: Request = serde_json::from_value(value).unwrap();
    assert_eq!(back.jsonrpc, "2.0");
    assert_eq!(back.method, "state.bootstrap");
    assert_eq!(
        serde_json::to_value(back.id).unwrap(),
        json!(7),
        "the request id must survive the round trip"
    );
    assert_eq!(back.params, json!({"since_revision": 41}));
}

#[test]
fn response_round_trips_through_a_frame() {
    let msg = sample_response();
    let value = round_trip(&msg);
    assert_eq!(value, serde_json::to_value(&msg).unwrap());
    let back: Response = serde_json::from_value(value).unwrap();
    assert_eq!(back.jsonrpc, "2.0");
    assert_eq!(back.result["revision"], json!(42));
}

#[test]
fn error_response_round_trips_through_a_frame() {
    let msg = sample_error_response();
    let value = round_trip(&msg);
    assert_eq!(value, serde_json::to_value(&msg).unwrap());
    let back: ErrorResponse = serde_json::from_value(value).unwrap();
    assert_eq!(back.error.code, ErrorCode::NotInitialized.code());
    assert_eq!(back.error.data, Some(json!({})));
}

#[test]
fn notification_round_trips_through_a_frame() {
    let msg = sample_notification();
    let value = round_trip(&msg);
    assert_eq!(value, serde_json::to_value(&msg).unwrap());
    let back: Notification = serde_json::from_value(value).unwrap();
    assert_eq!(back.method, "state.resync_required");
    assert_eq!(back.params["reason"], json!("event_queue_overflow"));
}

#[test]
fn event_envelope_round_trips_through_a_frame() {
    let msg = sample_event();
    let value = round_trip(&msg);
    assert_eq!(value, serde_json::to_value(&msg).unwrap());
    let back: EventEnvelope = serde_json::from_value(value).unwrap();
    assert_eq!(back.revision, 42);
    assert_eq!(back.kind, "message.flags_changed");
    assert_eq!(back.instance_id, "01J000000000000000000000");
}

/// An event delivered as the `state.event` notification the plan specifies:
/// `params` is the envelope verbatim.
#[test]
fn event_envelope_nests_inside_a_state_event_notification() {
    let event = sample_event();
    let notification = Notification {
        jsonrpc: "2.0".to_string(),
        method: "state.event".to_string(),
        params: serde_json::to_value(&event).unwrap(),
    };
    let value = round_trip(&notification);
    assert_eq!(value["method"], json!("state.event"));
    let back: EventEnvelope = serde_json::from_value(value["params"].clone()).unwrap();
    assert_eq!(back.revision, event.revision);
    assert_eq!(back.kind, event.kind);
}

// ---------------------------------------------------------------------------
// Framing: incremental decoding
// ---------------------------------------------------------------------------

/// A frame arriving in three chunks yields nothing until the terminator lands.
#[test]
fn a_frame_split_across_three_pushes_yields_one_value_at_the_end() {
    let bytes = frame::encode(&sample_request()).unwrap();
    assert!(bytes.len() > 6, "need a frame long enough to cut in three");
    let first = bytes.len() / 3;
    let second = (bytes.len() * 2) / 3;

    let mut decoder = Decoder::new(MAX_REQUEST_BYTES);
    assert!(
        decoder.push(&bytes[..first]).unwrap().is_empty(),
        "a partial frame must not yield a value"
    );
    assert!(
        decoder.push(&bytes[first..second]).unwrap().is_empty(),
        "still partial after the second chunk"
    );
    let values = decoder.push(&bytes[second..]).unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0], serde_json::to_value(sample_request()).unwrap());
}

/// The decoder must not lose the second message when two frames arrive in one
/// read, and must return them in arrival order.
#[test]
fn two_frames_in_one_push_yield_two_values_in_order() {
    let mut bytes = frame::encode(&sample_request()).unwrap();
    bytes.extend_from_slice(&frame::encode(&sample_notification()).unwrap());

    let mut decoder = Decoder::new(MAX_REQUEST_BYTES);
    let values = decoder.push(&bytes).unwrap();
    assert_eq!(values.len(), 2, "both frames must come back from one push");
    assert_eq!(values[0], serde_json::to_value(sample_request()).unwrap());
    assert_eq!(
        values[1],
        serde_json::to_value(sample_notification()).unwrap()
    );
}

/// Two complete frames plus the head of a third: the third stays buffered.
#[test]
fn a_trailing_partial_frame_stays_buffered() {
    let mut bytes = frame::encode(&sample_request()).unwrap();
    bytes.extend_from_slice(&frame::encode(&sample_response()).unwrap());
    let third = frame::encode(&sample_notification()).unwrap();
    bytes.extend_from_slice(&third[..third.len() / 2]);

    let mut decoder = Decoder::new(MAX_REQUEST_BYTES);
    assert_eq!(decoder.push(&bytes).unwrap().len(), 2);
    let rest = decoder.push(&third[third.len() / 2..]).unwrap();
    assert_eq!(rest.len(), 1, "the buffered head must join its tail");
    assert_eq!(
        rest[0],
        serde_json::to_value(sample_notification()).unwrap()
    );
}

#[test]
fn an_empty_push_yields_nothing_and_is_not_an_error() {
    let mut decoder = Decoder::new(MAX_REQUEST_BYTES);
    assert!(decoder.push(&[]).unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// Framing: the byte cap
// ---------------------------------------------------------------------------

/// Build a frame whose encoded length, terminator included, is exactly
/// `total_bytes`, by padding a string field. Padding is ASCII `x`, which JSON
/// never escapes, so one pad character costs exactly one byte.
fn padded_frame(total_bytes: usize) -> Vec<u8> {
    let base = Request {
        jsonrpc: "2.0".to_string(),
        id: Some(RequestId::Num(1)),
        method: "state.bootstrap".to_string(),
        params: json!({"pad": ""}),
    };
    let base_len = frame::encode(&base).unwrap().len();
    assert!(
        total_bytes >= base_len,
        "cannot build a frame shorter than the empty-pad frame ({base_len} bytes)"
    );
    let pad = "x".repeat(total_bytes - base_len);
    let msg = Request {
        params: json!({ "pad": pad }),
        ..base
    };
    let bytes = frame::encode(&msg).unwrap();
    assert_eq!(
        bytes.len(),
        total_bytes,
        "padding arithmetic is off; JSON escaped a pad character"
    );
    bytes
}

/// The cap counts the terminator: exactly `MAX_REQUEST_BYTES` is legal.
#[test]
fn a_frame_of_exactly_the_limit_is_accepted() {
    let bytes = padded_frame(MAX_REQUEST_BYTES);
    let mut decoder = Decoder::new(MAX_REQUEST_BYTES);
    let values = decoder.push(&bytes).expect("a frame at the limit is legal");
    assert_eq!(values.len(), 1);
}

#[test]
fn a_frame_over_the_limit_is_too_large() {
    let bytes = padded_frame(MAX_REQUEST_BYTES + 1);
    let mut decoder = Decoder::new(MAX_REQUEST_BYTES);
    match decoder.push(&bytes) {
        Err(FrameError::TooLarge { limit, seen }) => {
            assert_eq!(limit, MAX_REQUEST_BYTES, "the reported limit is the cap");
            assert!(
                seen > limit,
                "`seen` must report the byte count that breached the cap, got {seen}"
            );
        }
        other => panic!("expected TooLarge, got {other:?}"),
    }
}

/// The cap is enforced on the buffer, not on a completed line: a client that
/// streams a gigabyte without ever sending `\n` must be cut off, not buffered.
#[test]
fn an_unterminated_run_over_the_limit_is_too_large() {
    let payload = vec![b'x'; MAX_REQUEST_BYTES + 1];
    let mut decoder = Decoder::new(MAX_REQUEST_BYTES);
    match decoder.push(&payload) {
        Err(FrameError::TooLarge { limit, seen }) => {
            assert_eq!(limit, MAX_REQUEST_BYTES);
            assert!(seen > limit, "got seen = {seen}");
        }
        other => panic!("expected TooLarge for an unterminated run, got {other:?}"),
    }
}

/// The decoder honours the limit it was constructed with, not the constant, so
/// a response decoder can carry a different cap (plan section 3.0: the response
/// cap is decided separately).
#[test]
fn the_decoder_honours_its_own_limit() {
    let mut decoder = Decoder::new(64);
    match decoder.push(&padded_frame(200)) {
        Err(FrameError::TooLarge { limit, seen }) => {
            assert_eq!(limit, 64);
            assert!(seen > 64, "got seen = {seen}");
        }
        other => panic!("expected TooLarge at a 64-byte cap, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Framing: malformed input
// ---------------------------------------------------------------------------

#[test]
fn invalid_utf8_is_rejected() {
    let mut decoder = Decoder::new(MAX_REQUEST_BYTES);
    // 0xff is never valid UTF-8 anywhere in a sequence.
    match decoder.push(&[b'{', 0xff, b'}', b'\n']) {
        Err(FrameError::InvalidUtf8) => {}
        other => panic!("expected InvalidUtf8, got {other:?}"),
    }
}

#[test]
fn invalid_json_is_rejected_with_a_message() {
    let mut decoder = Decoder::new(MAX_REQUEST_BYTES);
    match decoder.push(b"not json at all\n") {
        Err(FrameError::InvalidJson(message)) => {
            assert!(
                !message.is_empty(),
                "InvalidJson must carry the parser's message for the daemon log"
            );
        }
        other => panic!("expected InvalidJson, got {other:?}"),
    }
}

#[test]
fn a_truncated_json_object_is_invalid_json_not_a_silent_drop() {
    let mut decoder = Decoder::new(MAX_REQUEST_BYTES);
    match decoder.push(b"{\"jsonrpc\":\"2.0\",\n") {
        Err(FrameError::InvalidJson(_)) => {}
        other => panic!("expected InvalidJson, got {other:?}"),
    }
}

/// A newline inside a string value is escaped by the JSON encoder, so it never
/// reaches the wire as a raw byte, and it survives the round trip intact. This
/// is the property that makes line framing safe for message bodies.
#[test]
fn an_escaped_newline_inside_a_string_survives_the_round_trip() {
    let body = "first line\nsecond line\r\nthird\ttab";
    let msg = Notification {
        jsonrpc: "2.0".to_string(),
        method: "state.event".to_string(),
        params: json!({"body": body, "subject": "re: \"quoted\"\nfolded"}),
    };
    let bytes = frame::encode(&msg).unwrap();
    assert_eq!(
        bytes.iter().filter(|b| **b == b'\n').count(),
        1,
        "the embedded newlines must be escaped, leaving only the terminator"
    );
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(
        text.contains("\\n"),
        "the encoder must emit the two-character escape"
    );

    let mut decoder = Decoder::new(MAX_REQUEST_BYTES);
    let values = decoder.push(&bytes).unwrap();
    assert_eq!(values.len(), 1);
    let back: Notification = serde_json::from_value(values[0].clone()).unwrap();
    assert_eq!(back.params["body"], json!(body));
    assert_eq!(back.params["subject"], json!("re: \"quoted\"\nfolded"));
}

// ---------------------------------------------------------------------------
// FrameError as an error type
// ---------------------------------------------------------------------------

/// `FrameError` crosses the crate boundary into the daemon's connection loop,
/// which logs it and maps `TooLarge` onto `ErrorCode::FrameTooLarge`, so it must
/// be a real `std::error::Error` with a non-empty `Display`.
#[test]
fn frame_error_is_a_std_error_with_a_display() {
    fn assert_std_error<T: std::error::Error>(_: &T) {}
    let too_large = FrameError::TooLarge {
        limit: MAX_REQUEST_BYTES,
        seen: MAX_REQUEST_BYTES + 1,
    };
    assert_std_error(&too_large);
    for error in [
        too_large,
        FrameError::InvalidUtf8,
        FrameError::InvalidJson("trailing comma".to_string()),
    ] {
        assert!(
            !error.to_string().is_empty(),
            "every FrameError variant needs a Display message"
        );
    }
}

/// The `frame_too_large` wire error carries `{limit, seen}`, the same pair the
/// decoder reports (plan section 3.0 error table).
#[test]
fn too_large_maps_onto_the_frame_too_large_wire_error() {
    let FrameError::TooLarge { limit, seen } = (FrameError::TooLarge {
        limit: MAX_REQUEST_BYTES,
        seen: MAX_REQUEST_BYTES + 12,
    }) else {
        unreachable!()
    };
    let wire = RpcError {
        code: ErrorCode::FrameTooLarge.code(),
        message: ErrorCode::FrameTooLarge.name().to_string(),
        data: Some(json!({"limit": limit, "seen": seen})),
    };
    assert_eq!(
        serde_json::to_value(&wire).unwrap(),
        json!({
            "code": -32004,
            "message": "frame_too_large",
            "data": {"limit": 1_048_576, "seen": 1_048_588}
        })
    );
}
