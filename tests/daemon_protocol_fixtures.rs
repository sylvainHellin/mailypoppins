//! Committed wire fixtures for every public request, response, error, and event
//! shape (#0120, unit P2-U2).
//!
//! This file is a **contract test** written before `mp-protocol` has any
//! contents and before `crates/mp-protocol/fixtures/` exists; unit P2-U3
//! creates both. It compiles only under `--features daemon`.
//!
//! Two rules the fixtures must satisfy, decided here so P2-U3 can follow them:
//!
//! 1. **Canonical formatting.** A fixture file is `serde_json::to_string_pretty`
//!    of its own content plus a trailing newline. `serde_json`'s default `Value`
//!    map is a `BTreeMap`, so that means two-space indentation and
//!    alphabetically ordered keys. This is what makes "re-serialises byte
//!    identically" a checkable property of a file on disk rather than of a
//!    struct's field order.
//! 2. **Lossless typing.** Parsing a fixture into its `mp_protocol` type and
//!    serialising that value back must reproduce the file's JSON exactly, as a
//!    `serde_json::Value` comparison. A field the type forgot would be dropped
//!    and a field the type invented would appear, and either fails here.
//!
//! The fixture-to-type mapping is by file name, which the plan fixes:
//! `error.*` is an `ErrorResponse`, `event.*` a bare `EventEnvelope`,
//! `notification.*` a `Notification`, `*.request.json` a `Request`, and
//! `*.response.json` a `Response`. A new fixture whose name matches none of
//! these fails rather than being skipped.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use mp_protocol::frame::{self, Decoder};
use mp_protocol::{
    ErrorCode, ErrorResponse, EventEnvelope, Notification, Request, Response, MAX_REQUEST_BYTES,
    PROTOCOL_MAX, PROTOCOL_MIN,
};

/// Relative to the root package's manifest directory, which is the repo root.
const FIXTURE_DIR: &str = "crates/mp-protocol/fixtures";

/// The eleven fixtures unit P2-U2 requires. Extra fixtures are welcome; a
/// missing one is a hole in the committed protocol surface.
const REQUIRED_FIXTURES: &[&str] = &[
    "account.list.request.json",
    "account.list.response.json",
    "error.identity_mismatch.json",
    "error.not_initialized.json",
    "error.protocol_incompatible.json",
    "event.envelope.json",
    "initialize.request.json",
    "initialize.response.json",
    "message.list.request.json",
    "message.list.response.json",
    "notification.resync_required.json",
];

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE_DIR)
}

/// Every `*.json` under the fixture directory, sorted by name.
///
/// Panics when the directory is missing or holds no fixtures: a fixture suite
/// that iterates an empty directory passes without testing anything, which is
/// exactly the vacuous test this unit is forbidden to produce.
fn fixtures() -> Vec<(String, PathBuf)> {
    let dir = fixture_dir();
    let entries = fs::read_dir(&dir).unwrap_or_else(|error| {
        panic!(
            "the protocol fixture directory {} is missing ({error}); \
             unit P2-U3 must create it with the {} fixtures listed in the plan",
            dir.display(),
            REQUIRED_FIXTURES.len()
        )
    });

    let mut found: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries {
        let path = entry.expect("read a fixture directory entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("fixture names are UTF-8")
            .to_string();
        found.push((name, path));
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));

    assert!(
        !found.is_empty(),
        "no *.json fixtures under {}; the fixture suite would pass vacuously",
        dir.display()
    );
    found
}

#[test]
fn every_required_fixture_is_committed() {
    let present: BTreeSet<String> = fixtures().into_iter().map(|(name, _)| name).collect();
    let missing: Vec<&&str> = REQUIRED_FIXTURES
        .iter()
        .filter(|name| !present.contains(**name))
        .collect();
    assert!(
        missing.is_empty(),
        "missing protocol fixtures: {missing:?} (present: {present:?})"
    );
    assert!(
        present.len() >= REQUIRED_FIXTURES.len(),
        "expected at least {} fixtures, found {}",
        REQUIRED_FIXTURES.len(),
        present.len()
    );
}

// ---------------------------------------------------------------------------
// Typing
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Request,
    Response,
    Error,
    Event,
    Notification,
}

fn kind_of(name: &str) -> Kind {
    if name.starts_with("error.") {
        Kind::Error
    } else if name.starts_with("event.") {
        Kind::Event
    } else if name.starts_with("notification.") {
        Kind::Notification
    } else if name.ends_with(".request.json") {
        Kind::Request
    } else if name.ends_with(".response.json") {
        Kind::Response
    } else {
        panic!(
            "fixture {name} matches no naming rule; name it `error.*`, `event.*`, \
             `notification.*`, `*.request.json` or `*.response.json`"
        )
    }
}

/// Parse into the `mp_protocol` type the file name selects, then serialise the
/// parsed value straight back to JSON.
fn retype(kind: Kind, name: &str, value: &Value) -> Value {
    fn parse<T: serde::de::DeserializeOwned + serde::Serialize>(
        name: &str,
        value: &Value,
    ) -> Value {
        let typed: T = serde_json::from_value(value.clone())
            .unwrap_or_else(|error| panic!("fixture {name} does not parse into its type: {error}"));
        serde_json::to_value(typed).expect("a protocol type always serialises")
    }
    match kind {
        Kind::Request => parse::<Request>(name, value),
        Kind::Response => parse::<Response>(name, value),
        Kind::Error => parse::<ErrorResponse>(name, value),
        Kind::Event => parse::<EventEnvelope>(name, value),
        Kind::Notification => parse::<Notification>(name, value),
    }
}

/// Every fixture parses into its type and serialises back to the identical
/// JSON. This is the "no field lost, no field invented" check.
#[test]
fn every_fixture_round_trips_through_its_type() {
    for (name, path) in fixtures() {
        let text = fs::read_to_string(&path).expect("read a fixture");
        let value: Value = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("fixture {name} is not valid JSON: {error}"));
        let kind = kind_of(&name);
        assert_eq!(
            retype(kind, &name, &value),
            value,
            "fixture {name} did not survive a {kind:?} round trip"
        );
    }
}

/// Every fixture file is stored canonically, so `re-serialise == the bytes on
/// disk` and a diff on a fixture is a protocol change rather than a reformat.
#[test]
fn every_fixture_is_stored_canonically() {
    for (name, path) in fixtures() {
        let text = fs::read_to_string(&path).expect("read a fixture");
        let value: Value = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("fixture {name} is not valid JSON: {error}"));
        let canonical = format!(
            "{}\n",
            serde_json::to_string_pretty(&value).expect("pretty-print a fixture")
        );
        assert_eq!(
            text, canonical,
            "fixture {name} is not canonical; write it as serde_json::to_string_pretty \
             (two-space indent, keys sorted) plus a trailing newline"
        );
    }
}

/// Every fixture fits in one frame and survives the wire unchanged, which is
/// what makes them usable as golden inputs for the daemon's connection loop.
#[test]
fn every_fixture_survives_a_frame_round_trip() {
    for (name, path) in fixtures() {
        let value: Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("read a fixture"))
                .expect("fixture is valid JSON");
        let bytes = frame::encode(&value)
            .unwrap_or_else(|error| panic!("fixture {name} does not encode into a frame: {error}"));
        assert!(
            bytes.len() <= MAX_REQUEST_BYTES,
            "fixture {name} does not fit in a single frame"
        );
        let mut decoder = Decoder::new(MAX_REQUEST_BYTES);
        let decoded = decoder
            .push(&bytes)
            .unwrap_or_else(|error| panic!("fixture {name} does not decode: {error}"));
        assert_eq!(decoded, vec![value], "fixture {name} changed on the wire");
    }
}

/// Every fixture that is a JSON-RPC message declares `"jsonrpc": "2.0"`. The
/// bare event envelope is not a JSON-RPC message and carries no such field.
#[test]
fn every_json_rpc_fixture_declares_version_two() {
    for (name, path) in fixtures() {
        let value: Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("read a fixture"))
                .expect("fixture is valid JSON");
        match kind_of(&name) {
            Kind::Event => assert!(
                value.get("jsonrpc").is_none(),
                "fixture {name} is a bare envelope, not a JSON-RPC message"
            ),
            _ => assert_eq!(
                value["jsonrpc"],
                json!("2.0"),
                "fixture {name} must declare jsonrpc 2.0"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Named shapes
// ---------------------------------------------------------------------------

fn load(name: &str) -> Value {
    let path = fixture_dir().join(name);
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("fixture {name} is missing: {error}"));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("fixture {name} is not JSON: {error}"))
}

fn assert_string(value: &Value, pointer: &str, name: &str) {
    let found = value
        .pointer(pointer)
        .unwrap_or_else(|| panic!("fixture {name} has no {pointer}"));
    assert!(
        found.as_str().is_some_and(|s| !s.is_empty()),
        "fixture {name}: {pointer} must be a non-empty string, got {found}"
    );
}

fn assert_object(value: &Value, pointer: &str, name: &str) {
    let found = value
        .pointer(pointer)
        .unwrap_or_else(|| panic!("fixture {name} has no {pointer}"));
    assert!(
        found.is_object(),
        "fixture {name}: {pointer} must be an object, got {found}"
    );
}

fn assert_array(value: &Value, pointer: &str, name: &str) {
    let found = value
        .pointer(pointer)
        .unwrap_or_else(|| panic!("fixture {name} has no {pointer}"));
    assert!(
        found.is_array(),
        "fixture {name}: {pointer} must be an array, got {found}"
    );
}

/// The initialize request carries everything the handshake needs: client kind
/// and version, the supported protocol range, required and optional
/// capabilities, and the data-dir / config-dir pair. The pair is sent always,
/// not only under an override, because `MAILYPOPPINS_CONFIG_DIR` and
/// `MAILYPOPPINS_DATA_DIR` are independent and a half-overridden client would
/// otherwise reach a daemon holding different config and secrets.
#[test]
fn initialize_request_carries_the_full_handshake() {
    let name = "initialize.request.json";
    let value = load(name);
    assert_eq!(value["method"], json!("initialize"));
    assert!(
        !value["id"].is_null(),
        "fixture {name}: initialize is a request and must carry an id"
    );

    assert_string(&value, "/params/client/type", name);
    assert_string(&value, "/params/client/version", name);
    assert_string(&value, "/params/identity/data_dir", name);
    assert_string(&value, "/params/identity/config_dir", name);
    assert_array(&value, "/params/capabilities/required", name);
    assert_array(&value, "/params/capabilities/optional", name);

    assert_eq!(
        value["params"]["protocol"]["min"],
        json!(PROTOCOL_MIN),
        "fixture {name}: the client's minimum must match PROTOCOL_MIN"
    );
    assert_eq!(
        value["params"]["protocol"]["max"],
        json!(PROTOCOL_MAX),
        "fixture {name}: the client's maximum must match PROTOCOL_MAX"
    );
}

/// The response names the daemon version, the selected protocol version, the
/// instance identifier, the supported capabilities, the platform and lifecycle
/// capabilities, and the current configuration status.
#[test]
fn initialize_response_carries_the_full_handshake() {
    let name = "initialize.response.json";
    let value = load(name);
    assert!(
        !value["id"].is_null(),
        "fixture {name}: a response carries the request's id"
    );
    assert!(
        value.get("error").is_none(),
        "fixture {name}: a success response has no error member"
    );

    assert_string(&value, "/result/daemon/version", name);
    assert_string(&value, "/result/instance_id", name);
    assert_array(&value, "/result/capabilities", name);
    assert_object(&value, "/result/platform", name);
    assert_object(&value, "/result/lifecycle", name);
    assert_object(&value, "/result/config_status", name);

    let selected = value["result"]["protocol"]["selected"]
        .as_u64()
        .unwrap_or_else(|| panic!("fixture {name}: /result/protocol/selected must be an integer"));
    assert!(
        (u64::from(PROTOCOL_MIN)..=u64::from(PROTOCOL_MAX)).contains(&selected),
        "fixture {name}: the selected version {selected} is outside the daemon's range"
    );
}

/// The error fixtures pin the numbers in the plan's error table, and each one's
/// `data` carries the shape that table fixes.
#[test]
fn error_fixtures_pin_the_code_table() {
    for (name, expected) in [
        ("error.not_initialized.json", ErrorCode::NotInitialized),
        ("error.identity_mismatch.json", ErrorCode::IdentityMismatch),
        (
            "error.protocol_incompatible.json",
            ErrorCode::ProtocolIncompatible,
        ),
    ] {
        let value = load(name);
        assert_eq!(
            value["error"]["code"],
            json!(expected.code()),
            "fixture {name} must carry code {} ({})",
            expected.code(),
            expected.name()
        );
        assert_string(&value, "/error/message", name);
        let typed: ErrorResponse =
            serde_json::from_value(value.clone()).expect("an error fixture is an ErrorResponse");
        assert_eq!(typed.error.code, expected.code());
    }
}

#[test]
fn not_initialized_carries_an_empty_data_object() {
    let name = "error.not_initialized.json";
    let value = load(name);
    assert_eq!(
        value["error"]["data"],
        json!({}),
        "fixture {name}: the table fixes `data` as `{{}}`"
    );
}

/// `{daemon:{data_dir,config_dir}, client:{data_dir,config_dir}}` -- both
/// directories on both sides, so the refusal message can name all four.
#[test]
fn identity_mismatch_names_both_directories_on_both_sides() {
    let name = "error.identity_mismatch.json";
    let value = load(name);
    for pointer in [
        "/error/data/daemon/data_dir",
        "/error/data/daemon/config_dir",
        "/error/data/client/data_dir",
        "/error/data/client/config_dir",
    ] {
        assert_string(&value, pointer, name);
    }
    assert_ne!(
        value.pointer("/error/data/daemon").unwrap(),
        value.pointer("/error/data/client").unwrap(),
        "fixture {name}: an identity mismatch fixture whose two sides agree \
         documents nothing"
    );
}

/// `{daemon:{min,max}, client:{min,max}}`, and the ranges must actually be
/// disjoint or the fixture illustrates a case that would have succeeded.
#[test]
fn protocol_incompatible_names_both_version_ranges() {
    let name = "error.protocol_incompatible.json";
    let value = load(name);
    let mut bounds = Vec::new();
    for side in ["daemon", "client"] {
        let mut pair = Vec::new();
        for bound in ["min", "max"] {
            let pointer = format!("/error/data/{side}/{bound}");
            let found = value
                .pointer(&pointer)
                .unwrap_or_else(|| panic!("fixture {name} has no {pointer}"));
            pair.push(
                found
                    .as_u64()
                    .unwrap_or_else(|| panic!("fixture {name}: {pointer} must be an integer")),
            );
        }
        assert!(
            pair[0] <= pair[1],
            "fixture {name}: the {side} range {pair:?} is empty"
        );
        bounds.push(pair);
    }
    let (daemon, client) = (&bounds[0], &bounds[1]);
    assert!(
        daemon[1] < client[0] || client[1] < daemon[0],
        "fixture {name}: the ranges {daemon:?} and {client:?} overlap, so this \
         handshake would have succeeded"
    );
}

/// The event envelope is `{instance_id, revision, kind, payload}` and nothing
/// else; the notification wrapper lives in `state.event`, not in this file.
#[test]
fn event_envelope_fixture_has_the_pinned_fields() {
    let name = "event.envelope.json";
    let value = load(name);
    let typed: EventEnvelope =
        serde_json::from_value(value.clone()).expect("the event fixture is an EventEnvelope");
    assert!(
        !typed.instance_id.is_empty(),
        "fixture {name}: instance_id identifies the daemon that produced the event"
    );
    assert!(
        typed.revision > 0,
        "fixture {name}: revision 0 is the pre-bootstrap sentinel"
    );
    assert!(
        !typed.kind.is_empty(),
        "fixture {name}: kind selects the client's handler"
    );
    assert!(
        value["payload"].is_object(),
        "fixture {name}: payload must be an object so it can grow fields"
    );
}

/// The control notification: method `state.resync_required`,
/// `params = {instance_id, reason}` (plan section 3.0).
#[test]
fn resync_required_notification_has_the_pinned_shape() {
    let name = "notification.resync_required.json";
    let value = load(name);
    assert_eq!(value["method"], json!("state.resync_required"));
    assert!(
        value.get("id").is_none(),
        "fixture {name}: a notification carries no id"
    );
    assert_string(&value, "/params/instance_id", name);
    assert_string(&value, "/params/reason", name);

    let typed: Notification = serde_json::from_value(value).expect("the fixture is a Notification");
    assert_eq!(typed.method, "state.resync_required");
}

/// Domain-method fixtures name a method in one of the families the plan lists,
/// and their responses answer a request id.
#[test]
fn domain_fixtures_use_the_declared_method_families() {
    const FAMILIES: &[&str] = &[
        "state",
        "account",
        "mailbox",
        "message",
        "draft",
        "send",
        "sync",
        "contact",
        "calendar",
        "signature",
        "config",
        "operation",
        "diagnostic",
        "daemon",
    ];
    for (name, method) in [
        ("account.list.request.json", "account.list"),
        ("message.list.request.json", "message.list"),
    ] {
        let value = load(name);
        assert_eq!(value["method"], json!(method));
        let family = method.split('.').next().unwrap();
        assert!(
            FAMILIES.contains(&family),
            "fixture {name}: `{family}.*` is not one of the declared method families"
        );
        let typed: Request = serde_json::from_value(value).expect("the fixture is a Request");
        assert!(
            typed.id.is_some(),
            "fixture {name}: a domain call is a request, not a notification"
        );
        assert!(
            typed.params.is_object(),
            "fixture {name}: params must be an object, never positional"
        );
    }

    for name in ["account.list.response.json", "message.list.response.json"] {
        let value = load(name);
        let typed: Response = serde_json::from_value(value).expect("the fixture is a Response");
        assert_eq!(typed.jsonrpc, "2.0");
        assert!(
            !typed.result.is_null(),
            "fixture {name}: a success response carries a result"
        );
    }
}

/// The read-only response fixtures carry exactly the fields
/// `docs/daemon-protocol.md` documents for them.
///
/// The key lists are written out here rather than read from the daemon's
/// `to_json`, so a field silently added, renamed or dropped on the wire fails
/// this test instead of regenerating a fixture that agrees with the new code
/// and with nothing else. Changing one of these lists is a protocol change and
/// needs a changelog entry.
#[test]
fn the_read_only_response_fixtures_carry_the_documented_fields() {
    const ACCOUNT_ENTRY: &[&str] = &["backend", "default", "name", "state"];
    const MESSAGE_ROW: &[&str] = &[
        "date_display",
        "date_sort",
        "flags",
        "from",
        "has_attachments",
        "message_id",
        "subject",
        "uid",
    ];
    const MESSAGE_FLAGS: &[&str] = &["answered", "forwarded", "seen"];

    let accounts = load("account.list.response.json");
    assert_keys(&accounts["result"], &["accounts"], "account.list result");
    let entries = accounts["result"]["accounts"]
        .as_array()
        .expect("account.list result.accounts is an array");
    assert!(
        entries.len() >= 2,
        "the fixture shows both a default and a non-default account, got {accounts}"
    );
    for (index, entry) in entries.iter().enumerate() {
        assert_keys(entry, ACCOUNT_ENTRY, &format!("accounts[{index}]"));
    }

    let messages = load("message.list.response.json");
    assert_keys(
        &messages["result"],
        &["account", "mailbox", "total", "messages"],
        "message.list result",
    );
    let rows = messages["result"]["messages"]
        .as_array()
        .expect("message.list result.messages is an array");
    assert!(
        !rows.is_empty(),
        "a fixture with no message documents no row shape, got {messages}"
    );
    for (index, row) in rows.iter().enumerate() {
        assert_keys(row, MESSAGE_ROW, &format!("messages[{index}]"));
        assert_keys(
            &row["flags"],
            MESSAGE_FLAGS,
            &format!("messages[{index}].flags"),
        );
    }
}

/// The `message.list` request carries the three params the method takes and no
/// fourth: `offset` and `since_revision` belong to later versions of the shape
/// and a fixture may not promise them.
#[test]
fn the_message_list_request_carries_the_documented_params() {
    let value = load("message.list.request.json");
    assert_keys(
        &value["params"],
        &["account", "mailbox", "limit"],
        "message.list params",
    );
    let value = load("account.list.request.json");
    assert_eq!(
        value["params"],
        json!({}),
        "account.list takes no parameters"
    );
}

/// The keys of a JSON object, sorted, or a failure naming what came instead.
fn assert_keys(value: &Value, expected: &[&str], label: &str) {
    let map = value
        .as_object()
        .unwrap_or_else(|| panic!("{label} is a JSON object, got {value}"));
    let mut found: Vec<&str> = map.keys().map(String::as_str).collect();
    found.sort_unstable();
    let mut want: Vec<&str> = expected.to_vec();
    want.sort_unstable();
    assert_eq!(
        found, want,
        "{label} carries exactly the documented fields, got {value}"
    );
}

/// `initialize` is deliberately not namespaced: it is the one method a client
/// may call before initialization, so it cannot live behind a family gate.
#[test]
fn initialize_is_the_only_unnamespaced_method() {
    for (name, path) in fixtures() {
        if kind_of(&name) != Kind::Request {
            continue;
        }
        let value: Value = serde_json::from_str(&fs::read_to_string(&path).expect("read"))
            .expect("fixture is valid JSON");
        let method = value["method"]
            .as_str()
            .unwrap_or_else(|| panic!("fixture {name} has no string method"));
        assert!(
            method == "initialize" || method.contains('.'),
            "fixture {name}: method `{method}` is neither `initialize` nor `family.name`"
        );
    }
}
