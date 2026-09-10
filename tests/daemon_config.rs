//! Config ownership, atomic reload and runtime reconciliation (#0122, plan
//! unit P3b-U7).
//!
//! This file is a **contract test**: it is written before
//! `src/daemon/methods/config.rs` and the two `mp-protocol` payload types
//! exist, against the contract fixed in
//! `.agents/workflow/native-gui-daemon/plan.md` section 3.4 (unit P3b-U7) and
//! the source plan's configuration-ownership prose ("the daemon is the only
//! component that parses complete configuration or opens secret backends … a
//! watcher parses a candidate configuration into a new immutable snapshot,
//! validates every account, and swaps it only on success … invalid
//! configuration leaves the previous valid configuration active and publishes a
//! diagnostic … a first run with no `config.toml` starts the daemon with zero
//! accounts and serves the `config.*` family in that state … secret mutation
//! methods accept secret values only on local protected connections and redact
//! all debug representations"). It does not compile under `--features daemon`
//! today, and that failure *is* the proof the contract has no stub behind it.
//! An implementer (P3b-U8) does not edit this file; they make it pass.
//!
//! # The two layers, and why the split
//!
//! **(a) In-process, against the new types.** The six method declarations, the
//! two event kinds and their payload shapes, the secret-key spelling and the
//! redacting `Debug` are properties of types rather than of a process. A
//! `Debug` implementation in particular is not observable over a socket at all:
//! the whole point of the assertion is that a value which never reaches the
//! wire also never reaches a log line, and only the type can be asked.
//!
//! **(b) Over the socket, against a spawned `mp daemon run`.** Whether a swap
//! is atomic, whether a rejected candidate leaves the previous snapshot live,
//! whether a removed account's engine lock comes back, and whether a secret
//! stays out of the daemon's log and stderr are properties of the daemon
//! process and of nothing smaller. The engine lock is cross-process by
//! construction, so the assertion is a `flock` attempt from the test process,
//! exactly as `tests/daemon_account_runtime.rs` makes it.
//!
//! # Surface under test
//!
//! ```rust,ignore
//! // crates/mp-protocol/src/events.rs  ->  mp_protocol::events
//! pub const KIND_CONFIG_CHANGED: &str = "config.changed";
//! pub const KIND_CONFIG_INVALID: &str = "config.invalid";
//!
//! #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
//! pub struct ConfigChanged {
//!     pub added: Vec<String>,
//!     pub updated: Vec<String>,
//!     pub removed: Vec<String>,
//!     pub config_revision: u64,
//! }
//!
//! #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
//! pub struct ConfigInvalid {
//!     pub path: String,
//!     pub line: Option<u32>,
//!     pub message: String,
//! }
//!
//! // src/daemon/methods/config.rs  ->  mailypoppins::daemon::methods::config
//! pub const REDACTED: &str = "<redacted>";
//! pub const CONFIG_METHOD_SPECS: [MethodSpec; 6];   // in name order
//!
//! #[derive(Clone, Copy, Debug, PartialEq, Eq)]
//! pub enum SecretKind { Smtp, Imap }
//! impl SecretKind {
//!     pub fn as_str(self) -> &'static str;              // "smtp" | "imap"
//!     pub fn from_wire(value: &str) -> Option<SecretKind>;
//!     pub fn secret_key(self, account: &str) -> String; // "smtp-password-<account>"
//! }
//!
//! #[derive(Clone)]                                      // Debug is hand-written
//! pub struct SetPasswordParams;
//! impl SetPasswordParams {
//!     pub fn new(account: impl Into<String>, kind: SecretKind, value: impl Into<String>) -> Self;
//!     pub fn from_params(params: &Value) -> Result<Self, DomainError>;
//!     pub fn account(&self) -> &str;
//!     pub fn kind(&self) -> SecretKind;
//!     pub fn value(&self) -> &str;
//! }
//! ```
//!
//! # The six methods, as the dispatcher declares them
//!
//! | name | kind | since | cancel scope |
//! |---|---|---:|---|
//! | `config.add_account` | `Command` | 1 | `Durable` |
//! | `config.get` | `Query` | 1 | `Durable` |
//! | `config.init` | `Command` | 1 | `Durable` |
//! | `config.reload` | `Command` | 1 | `Durable` |
//! | `config.set_password` | `Command` | 1 | `Durable` |
//! | `config.validate` | `Query` | 1 | `Durable` |
//!
//! Every one of them is `Durable`: a configuration swap that undid itself
//! because the client that asked for it exited would leave the daemon serving a
//! configuration nobody chose, and the plan's rule is that a disconnect
//! cancels only what a `MethodSpec` declares client-scoped.
//!
//! # The wire shapes this file pins
//!
//! ```jsonc
//! // config.get   (no parameters)
//! {"revision": u64, "path": str, "state": "ok"|"absent"|"invalid", "config": {…}}
//!
//! // config.validate  {"toml": str}
//! {"ok": true}
//! {"ok": false, "errors": [{"line": u32|null, "message": str}]}
//!
//! // config.reload  (no parameters)
//! {"added": [str], "updated": [str], "removed": [str]}
//!
//! // config.set_password  {"account": str, "kind": "smtp"|"imap", "value": str}
//! {"stored": true, "account": str, "kind": "smtp"|"imap", "key": str}
//!
//! // config.add_account  {"account": {…}}
//! {"added": [str], "updated": [str], "removed": [str]}
//!
//! // config.init  {"account": {…}, "secrets_backend"?, "theme"?, "notifications"?}
//! {"added": [str], "updated": [str], "removed": [str], "path": str}
//!
//! // event config.changed
//! {"added": [str], "updated": [str], "removed": [str], "config_revision": u64}
//!
//! // event config.invalid
//! {"path": str, "line": u32|null, "message": str}
//! ```
//!
//! # Contract points this file pins beyond the plan text
//!
//! The plan fixes six method names, two params objects and two event kinds and
//! leaves the rest to the unit that pins it. These are the decisions taken
//! here, and the reasoning behind each.
//!
//! - **`config.get` wraps the configuration rather than being it.** The plan
//!   says "redacted effective config"; the result is
//!   `{revision, path, state, config}` because a client that reads a
//!   configuration needs to know *which* one it read: the `revision` is the
//!   same counter [`ConfigChanged::config_revision`] carries, so a client can
//!   tell whether the copy it holds predates the swap it was just told about.
//!   `path` and `state` are the same two facts `initialize`'s `config_status`
//!   reports, spelled the same way, so a client that connected before a
//!   `config.init` does not have to reconnect to learn the file now exists.
//! - **The config revision starts at 0 and increments by one per successful
//!   swap.** The configuration the daemon loaded at startup - including "no
//!   configuration at all" - is revision 0. It is not the state revision:
//!   state revisions move on every event from every source, and a client
//!   comparing configuration copies needs a counter that moves only when a
//!   configuration does.
//! - **A rejected candidate does not move the config revision**, because
//!   nothing was swapped. `config.invalid` therefore carries no revision at
//!   all: there is no new one to name.
//! - **"Effective" means after serde defaults, not after the engine's clamps.**
//!   A `config.toml` that omits `smtp.port` reports `465`, because that is what
//!   [`mailypoppins::config::GlobalConfig`] holds once it has loaded, and a
//!   client that showed an empty port would be showing something the daemon
//!   does not use. The `[1, 8]` and `[0, 600]` clamps on
//!   `imap.fetch_concurrency` and `imap.body_fetch_deadline_secs` belong to
//!   `ImapConfig::load`, which needs credentials and is not on this path, so
//!   `config.get` reports the loaded value and not the clamped one. Every test
//!   below stays inside both ranges, so the two readings never differ here.
//! - **Retention is reported resolved.** Per-account overrides layered over the
//!   global table, every optional filled in, which is
//!   [`mailypoppins::config::RetentionPolicy`] and is what the engine acts on.
//!   Reporting the raw `Option`s would make a client re-implement
//!   `RetentionPolicy::resolve` to show a user a number.
//! - **The secret-bearing fields are `smtp.password` and `imap.password`, and
//!   they are always present and always the literal [`REDACTED`].** Present,
//!   because their absence would say "no password is stored" - which is a fact
//!   about the secrets backend, and a fact `config.get` must not go and look
//!   up, since answering it for every account on every read is exactly the
//!   pattern that turns a redacted read into a secret read. Always the literal,
//!   because a length, a prefix or a fixed number of asterisks all leak
//!   something. `oauth2.client_id` is **not** redacted: it is a public
//!   identifier, `mp config show` prints it in the clear today, and redacting
//!   it would break the one screen that exists to diagnose an OAuth2 setup.
//! - **`config.validate` is a pure function of the string it is given.** It
//!   reads no file, writes no file, swaps nothing and moves no revision: it is
//!   the "would this load?" a GUI asks while the user is still typing. It runs
//!   the same three checks `load_global_config` runs - legacy-key rejection,
//!   the TOML parse into `GlobalConfig`, retention validation - against the
//!   parameter instead of against the file.
//! - **A `line` is the 1-based line of the offending token, and `null` when the
//!   diagnostic has no position.** A TOML syntax error carries a span
//!   (`toml::de::Error::span`), and the line is the number of `\n` before
//!   `span.start`, plus one. A semantic refusal - a retention horizon out of
//!   range, a removed key - has no span, so it reports `null` rather than
//!   inventing a position. That is why the plan writes `u32|null` rather than
//!   `u32`.
//! - **`errors` is never empty when `ok` is false, and this file pins only its
//!   first entry.** A TOML parser stops at the first syntax error and a
//!   semantic pass may or may not continue past the first refusal; requiring
//!   every error at once would pin the parser rather than the contract.
//! - **The reconciliation order is stop-removed, then update, then start-added,
//!   and it is observable as an ordered event sequence.** A removed account
//!   publishes `state.remove` of `account:<name>`; an updated or added one
//!   publishes `account.state_changed` when its runtime settles; `config.changed`
//!   is published last, after every per-account event of that swap. So the swap
//!   is not "eventually consistent" from a client's point of view: everything
//!   the reload did is in front of the `config.changed` that announces it. The
//!   order matters beyond tidiness - releasing a removed account's engine lock
//!   before an added account tries to take one is what lets an account be
//!   renamed in one edit without the new runtime losing a race to the old one.
//! - **`config.reload` answers only once every runtime it touched has settled.**
//!   Stopped ones are dropped, started ones are `ready` or `blocked`. A reload
//!   that returned while its runtimes were still opening would make "the lock
//!   is free again" a thing a caller has to poll for, and the caller that most
//!   needs it is a rename, which needs it synchronously.
//! - **An account is *updated* when its effective JSON changed**, that is when
//!   the object `config.get` reports for it differs from the one the previous
//!   snapshot reported. Comparing the loaded structs would need a `PartialEq`
//!   the config types do not have, and comparing the raw TOML text would call a
//!   reformatted file a change.
//! - **The three lists are sorted lexicographically** in the result and in the
//!   event, so two daemons reconciling the same edit report it identically and
//!   a test can assert a list rather than a set.
//! - **Every successful swap publishes exactly one `config.changed`, even when
//!   all three lists are empty.** A client that asked for a reload learns it
//!   happened; a no-op reload is a fact, not silence. It is a lifecycle event,
//!   so two swaps never coalesce into one: the second would otherwise hide the
//!   first swap's lists from a client that was slow to read, and the lists are
//!   the whole payload.
//! - **A runtime is started against an account directory the daemon creates if
//!   it is missing.** `mp config init` already promises this
//!   (`src/config_cmd/init.rs::print_account_data_paths`, "The directory itself
//!   is created here"), and an account added by a hand edit must come up the
//!   same way as one added through `config.add_account`.
//! - **`config.init` and `config.add_account` carry no secret.** The wizard
//!   they mirror collects a password in the same pass, and the daemon-era
//!   equivalent is two calls: the account, then `config.set_password`. One path
//!   into the secrets backend is one path to audit, and a secret inside a
//!   params object that a daemon may reasonably log as "the account that was
//!   added" is a secret in a log line waiting to happen.
//! - **`config.init` refuses an existing `config.toml` with `-32602` naming the
//!   path.** `mp config init` asks "Overwrite? [y/N]" and a daemon has nobody
//!   to ask; the daemon table has no "already exists" code, and inventing one
//!   in a T unit would put a number on the wire that the protocol fixtures
//!   (P2-U2) never pinned. The file is left byte-identical.
//! - **`config.add_account` without a `config.toml` is `-32602` naming
//!   `config.init`**, for the mirror-image reason: appending to a file that
//!   does not exist is `config.init`'s job, and guessing every global setting
//!   on the caller's behalf is exactly what the wizard exists not to do.
//! - **`config.set_password` publishes no event and reports the current state
//!   revision.** A stored password changes nothing a client can observe -
//!   `config.get` said `<redacted>` before and says `<redacted>` after - so
//!   there is nothing to fan out. It stays a `Command` rather than a `Query`
//!   because it writes, and its `affected` names `account:<name>` so a client
//!   holding a per-account view knows to re-read whatever depends on
//!   credentials.
//! - **The secret key is the one the existing backend already uses**,
//!   `smtp-password-<account>` and `imap-password-<account>`
//!   (`src/config_cmd/init.rs`, `src/config.rs::SmtpConfig::load`). A daemon
//!   that invented its own key namespace would store a password `mp config
//!   show` and the pre-daemon binary cannot find.
//! - **An unknown account is `-32005` with `{"account": name}`**, the daemon
//!   table's `account_unknown`, and its message and payload must not echo the
//!   value: an error message is the single most likely place for a secret to
//!   escape, because it is rendered, logged and often pasted into a bug report.
//! - **A `Debug` of [`SetPasswordParams`] prints [`REDACTED`] where the value
//!   is**, and still prints the account and the kind. `#[derive(Debug)]` on a
//!   struct holding the value is the failure this pins against, and it is worth
//!   pinning precisely because it is invisible until the day someone writes
//!   `debug!("{params:?}")`.
//! - **A rejected reload answers `-32007` with `{path, line?, message}`**, the
//!   daemon table's `config_invalid` and the payload that table fixes, and the
//!   same three fields travel as the `config.invalid` event. One shape, so a
//!   client that renders a diagnostic renders it the same way whether it asked
//!   for the reload or merely watched one.
//! - **The six specs live in one const array the family registers itself
//!   from.** `CONFIG_METHOD_SPECS` is what a test can read without building a
//!   daemon's state, and the socket-level capability assertion is what ties it
//!   to what is actually served: a method registered under a name that is not
//!   in the array is a capability the array does not name, and
//!   [`the_capability_list_is_exactly_the_declared_config_family`] fails.
//!
//! # Process and thread hygiene
//!
//! Every daemon started here is killed before the test returns, including on
//! panic: the child goes into a [`Proc`] whose `Drop` kills and reaps it, and
//! [`Sandbox`]'s `Drop` kills whatever `daemon.pid` names. Every wait is a
//! bounded poll or a `tokio::time::timeout`. Tests never touch the test
//! process's own environment: `HOME`, `MAILYPOPPINS_DATA_DIR` and
//! `MAILYPOPPINS_CONFIG_DIR` are passed to the child through `Command::env`,
//! and the four test-only hooks are explicitly cleared so an inherited variable
//! cannot change an outcome.

use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::net::UnixStream;

use mp_protocol::events::{ConfigChanged, ConfigInvalid, KIND_CONFIG_CHANGED, KIND_CONFIG_INVALID};
use mp_protocol::{ErrorCode, EventEnvelope, RpcError, METHOD_STATE_EVENT};

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity, InitializeResult};

use mailypoppins::config::RetentionPolicy;
use mailypoppins::daemon::dispatch::{CancelScope, MethodKind};
use mailypoppins::daemon::methods::config::{
    SecretKind, SetPasswordParams, CONFIG_METHOD_SPECS, REDACTED,
};
use mailypoppins::engine_lock::EngineLock;

const MP: &str = env!("CARGO_BIN_EXE_mp");

/// Upper bound on any single wait: a socket appearing, a reload answering, an
/// event arriving. Generous, because it is a ceiling and never a sleep.
const DEADLINE: Duration = Duration::from_secs(20);

/// Poll interval for every bounded wait.
const TICK: Duration = Duration::from_millis(25);

/// The environment opt-in for account runtimes (plan section 3.0). Spelled out
/// rather than imported so the test states the name a user would type.
const ACCOUNT_RUNTIMES_ENV: &str = "MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES";

/// The value every secrecy assertion hunts for. Distinctive enough that a
/// substring match cannot hit anything else the daemon writes.
const SECRET: &str = "s3cr3t-vodka-2f7a-never-logged";

/// The wire names of the six methods, in the order
/// [`CONFIG_METHOD_SPECS`] declares them.
const CONFIG_METHODS: [&str; 6] = [
    "config.add_account",
    "config.get",
    "config.init",
    "config.reload",
    "config.set_password",
    "config.validate",
];

/// The top-level keys of a `config.get` result, and no others.
const GET_KEYS: [&str; 4] = ["config", "path", "revision", "state"];

/// The keys of the `config` object inside it.
const CONFIG_KEYS: [&str; 6] = [
    "accounts",
    "email",
    "notifications",
    "retention",
    "secrets_backend",
    "theme",
];

/// The keys of one account inside that.
const ACCOUNT_KEYS: [&str; 9] = [
    "auth_method",
    "default_from",
    "imap",
    "mailboxes",
    "name",
    "oauth2",
    "retention",
    "save_to_sent",
    "smtp",
];

/// The keys of an account's `smtp` object. `password` is here and is always
/// [`REDACTED`].
const SMTP_KEYS: [&str; 5] = [
    "accept_invalid_certs",
    "host",
    "password",
    "port",
    "username",
];

/// The keys of an account's `imap` object.
const IMAP_KEYS: [&str; 7] = [
    "accept_invalid_certs",
    "body_fetch_deadline_secs",
    "fetch_concurrency",
    "host",
    "password",
    "port",
    "username",
];

/// The keys of an account's `mailboxes` object.
const MAILBOX_KEYS: [&str; 4] = ["archive", "extra", "inbox", "sent"];

/// The keys of a resolved retention policy, global or per account.
const RETENTION_KEYS: [&str; 4] = [
    "attachment_horizon_days",
    "body_horizon_days",
    "max_disk_bytes",
    "metadata_horizon_days",
];

/// The keys of the `email` object.
const EMAIL_KEYS: [&str; 4] = [
    "font_family",
    "font_size",
    "include_signature",
    "send_hold_secs",
];

/// The keys of a `config.reload` and `config.add_account` result.
const RECONCILE_KEYS: [&str; 3] = ["added", "removed", "updated"];

/// The default SMTP port `src/config.rs::default_smtp_port` applies to a file
/// that omits it. Spelled out rather than imported: the point of the assertion
/// is that `config.get` reports the *effective* value, and importing the
/// constant would make the test agree with the daemon by construction.
const DEFAULT_SMTP_PORT: u64 = 465;

/// The same for IMAP (`default_imap_port`).
const DEFAULT_IMAP_PORT: u64 = 993;

/// `default_fetch_concurrency`.
const DEFAULT_FETCH_CONCURRENCY: u64 = 4;

/// `default_body_fetch_deadline_secs`.
const DEFAULT_BODY_FETCH_DEADLINE_SECS: u64 = 30;

/// `default_send_hold_secs` (#0090).
const DEFAULT_SEND_HOLD_SECS: u64 = 20;

/// The line of a candidate document that is not TOML at all, used by the
/// syntax-error tests. Its 1-based line number in the document is computed from
/// the document rather than hard-coded, so the assertion states the rule
/// ("the line of the offending token") and not an arithmetic result.
const BROKEN_LINE: &str = "this line is not a key = value = pair";

// ---------------------------------------------------------------------------
// Layer (a) - the declarations
// ---------------------------------------------------------------------------

#[test]
fn the_redaction_literal_is_the_documented_string() {
    assert_eq!(
        REDACTED, "<redacted>",
        "one fixed literal, carrying neither the length nor the shape of what it hides"
    );
}

#[test]
fn the_two_event_kinds_are_the_documented_strings() {
    assert_eq!(KIND_CONFIG_CHANGED, "config.changed");
    assert_eq!(KIND_CONFIG_INVALID, "config.invalid");
}

#[test]
fn the_six_methods_declare_their_name_kind_since_and_cancel_scope() {
    let names: Vec<&str> = CONFIG_METHOD_SPECS.iter().map(|spec| spec.name).collect();
    assert_eq!(
        names,
        CONFIG_METHODS.to_vec(),
        "the six methods the plan names, in the name order the dispatcher's table keeps"
    );

    let kind_of = |name: &str| {
        CONFIG_METHOD_SPECS
            .iter()
            .find(|spec| spec.name == name)
            .unwrap_or_else(|| panic!("{name} is declared"))
    };

    assert_eq!(
        kind_of("config.get").kind,
        MethodKind::Query,
        "reading the effective configuration changes nothing"
    );
    assert_eq!(
        kind_of("config.validate").kind,
        MethodKind::Query,
        "validating a candidate string touches neither disk nor state"
    );
    for name in [
        "config.reload",
        "config.set_password",
        "config.add_account",
        "config.init",
    ] {
        assert_eq!(
            kind_of(name).kind,
            MethodKind::Command,
            "{name} changes daemon-owned state at once and reports the revision it moved to"
        );
    }

    for spec in CONFIG_METHOD_SPECS.iter() {
        assert_eq!(
            spec.since, 1,
            "{} is served by the first protocol version",
            spec.name
        );
        assert_eq!(
            spec.cancel_scope,
            CancelScope::Durable,
            "{} outlives the connection that asked for it: a configuration swap undone by a \
             disconnect would leave the daemon serving a configuration nobody chose",
            spec.name
        );
    }
}

#[test]
fn a_secret_kind_travels_as_its_wire_word_and_names_the_existing_backend_key() {
    assert_eq!(SecretKind::Smtp.as_str(), "smtp");
    assert_eq!(SecretKind::Imap.as_str(), "imap");
    assert_eq!(SecretKind::from_wire("smtp"), Some(SecretKind::Smtp));
    assert_eq!(SecretKind::from_wire("imap"), Some(SecretKind::Imap));
    assert_eq!(
        SecretKind::from_wire("pop3"),
        None,
        "the two kinds the plan names, and nothing else"
    );
    assert_eq!(
        SecretKind::from_wire("SMTP"),
        None,
        "a wire word is one spelling, as it is everywhere else in the protocol"
    );

    assert_eq!(
        SecretKind::Smtp.secret_key("alpha"),
        "smtp-password-alpha",
        "the key `src/config_cmd/init.rs` and `SmtpConfig::load` already use"
    );
    assert_eq!(
        SecretKind::Imap.secret_key("alpha"),
        "imap-password-alpha",
        "and its IMAP counterpart, which `ImapConfig::load` reads before falling back to SMTP"
    );
}

#[test]
fn a_debug_of_the_set_password_params_prints_the_redaction_literal() {
    let params = SetPasswordParams::new("alpha", SecretKind::Smtp, SECRET);
    assert_eq!(params.account(), "alpha");
    assert_eq!(params.kind(), SecretKind::Smtp);
    assert_eq!(
        params.value(),
        SECRET,
        "the value is readable by the one caller that stores it"
    );

    let rendered = format!("{params:?}");
    assert!(
        !rendered.contains(SECRET),
        "a Debug of the params must not carry the value: {rendered}"
    );
    assert!(
        rendered.contains(REDACTED),
        "and it says so with the redaction literal rather than by omitting the field: {rendered}"
    );
    assert!(
        rendered.contains("alpha"),
        "the account is still debuggable: {rendered}"
    );
    assert!(
        rendered.contains("Smtp") || rendered.contains("smtp"),
        "and so is the kind: {rendered}"
    );
}

#[test]
fn set_password_params_parse_from_the_wire_and_refuse_an_unknown_kind() {
    let parsed = SetPasswordParams::from_params(&json!({
        "account": "alpha", "kind": "imap", "value": SECRET
    }))
    .expect("a well-formed params object parses");
    assert_eq!(parsed.account(), "alpha");
    assert_eq!(parsed.kind(), SecretKind::Imap);
    assert_eq!(parsed.value(), SECRET);

    let refused = SetPasswordParams::from_params(&json!({
        "account": "alpha", "kind": "pop3", "value": SECRET
    }))
    .expect_err("a kind outside {smtp, imap} is refused");
    assert_eq!(
        refused.code(),
        -32602,
        "a parameter the daemon cannot read is invalid_params"
    );
    assert!(
        !refused.message().contains(SECRET),
        "a refusal must not echo the value it was handed: {}",
        refused.message()
    );
    assert!(
        !refused
            .data()
            .map(|data| data.to_string())
            .unwrap_or_default()
            .contains(SECRET),
        "and neither must its payload"
    );

    let missing = SetPasswordParams::from_params(&json!({"account": "alpha", "kind": "smtp"}))
        .expect_err("the value is required");
    assert_eq!(missing.code(), -32602);
}

#[test]
fn the_config_changed_payload_is_the_three_lists_and_the_new_revision() {
    let payload = ConfigChanged {
        added: vec!["gamma".to_string()],
        updated: vec!["beta".to_string()],
        removed: vec!["alpha".to_string()],
        config_revision: 7,
    };
    let json = serde_json::to_value(&payload).expect("a config.changed payload serialises");
    assert_eq!(
        sorted_keys(&json),
        vec!["added", "config_revision", "removed", "updated"],
        "four keys, and no rendered sentence among them"
    );
    assert_eq!(json["added"], json!(["gamma"]));
    assert_eq!(json["updated"], json!(["beta"]));
    assert_eq!(json["removed"], json!(["alpha"]));
    assert_eq!(json["config_revision"], json!(7));

    let back: ConfigChanged = serde_json::from_value(json).expect("and deserialises");
    assert_eq!(back, payload, "the payload round trips unchanged");
}

#[test]
fn the_config_invalid_payload_names_the_file_and_may_have_no_line() {
    let positioned = ConfigInvalid {
        path: "/c/config.toml".to_string(),
        line: Some(4),
        message: "expected `=`".to_string(),
    };
    let json = serde_json::to_value(&positioned).expect("a config.invalid payload serialises");
    assert_eq!(
        sorted_keys(&json),
        vec!["line", "message", "path"],
        "the three fields the plan names"
    );
    assert_eq!(json["line"], json!(4));
    assert_eq!(
        serde_json::from_value::<ConfigInvalid>(json).expect("round trips"),
        positioned
    );

    let unpositioned = ConfigInvalid {
        path: "/c/config.toml".to_string(),
        line: None,
        message: "[retention] metadata_horizon_days is out of range".to_string(),
    };
    let json = serde_json::to_value(&unpositioned).expect("serialises");
    assert_eq!(
        json["line"],
        Value::Null,
        "a diagnostic with no span reports null rather than inventing a position"
    );
    assert_eq!(
        sorted_keys(&json),
        vec!["line", "message", "path"],
        "the key is present and null, so a client never has to tell absent from unpositioned"
    );
    assert_eq!(
        serde_json::from_value::<ConfigInvalid>(json).expect("round trips"),
        unpositioned
    );
}

// ---------------------------------------------------------------------------
// Layer (b) - the sandbox
// ---------------------------------------------------------------------------

/// A private `HOME`, config directory and data directory.
///
/// Dropping it kills whatever daemon `daemon.pid` names, so nothing outlives
/// the test that started it, even when the test panics half way through.
struct Sandbox {
    root: TempDir,
}

impl Sandbox {
    /// A sandbox with no `config.toml` at all: the first-run case.
    fn empty() -> Self {
        let root = TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data"] {
            fs::create_dir_all(root.path().join(sub)).expect("sandbox subdir");
        }
        Sandbox { root }
    }

    /// A sandbox whose `config.toml` declares `accounts`, each with its data
    /// directory already present.
    fn with_accounts(accounts: &[&str]) -> Self {
        let sandbox = Sandbox::empty();
        let document: String = accounts
            .iter()
            .map(|name| account_toml(name, DEFAULT_BODY_FETCH_DEADLINE_SECS))
            .collect();
        sandbox.write_config(&document);
        for name in accounts {
            fs::create_dir_all(sandbox.account_dir(name)).expect("account dir");
        }
        sandbox
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn config_dir(&self) -> PathBuf {
        self.root.path().join("config")
    }

    fn data_dir(&self) -> PathBuf {
        self.root.path().join("data")
    }

    fn config_path(&self) -> PathBuf {
        self.config_dir().join("config.toml")
    }

    fn secrets_path(&self) -> PathBuf {
        self.config_dir().join("secrets.enc")
    }

    fn logs_dir(&self) -> PathBuf {
        self.data_dir().join("logs")
    }

    fn account_dir(&self, account: &str) -> PathBuf {
        self.data_dir().join("accounts").join(account)
    }

    fn lock_path(&self, account: &str) -> PathBuf {
        self.account_dir(account).join("store.lock")
    }

    fn socket(&self) -> PathBuf {
        self.data_dir().join("runtime").join("daemon.sock")
    }

    fn pid_file(&self) -> PathBuf {
        self.data_dir().join("runtime").join("daemon.pid")
    }

    fn identity(&self) -> Identity {
        Identity {
            data_dir: self.data_dir(),
            config_dir: self.config_dir(),
        }
    }

    fn write_config(&self, document: &str) {
        fs::write(self.config_path(), document).expect("write config.toml");
    }

    fn read_config(&self) -> String {
        fs::read_to_string(self.config_path()).expect("read config.toml")
    }

    /// An `mp` invocation pointed at this sandbox, with every test-only hook
    /// explicitly cleared so an inherited variable cannot change an outcome.
    fn cmd(&self) -> Command {
        let mut cmd = Command::new(MP);
        cmd.env("HOME", self.home())
            .env("MAILYPOPPINS_DATA_DIR", self.data_dir())
            .env("MAILYPOPPINS_CONFIG_DIR", self.config_dir())
            .env_remove(ACCOUNT_RUNTIMES_ENV)
            .env_remove("MAILYPOPPINS_DAEMON_FAIL_START")
            .env_remove("MAILYPOPPINS_DAEMON_FAKE_READY_AFTER_MS")
            .env_remove("MAILYPOPPINS_DAEMON_FAKE_EVENT_BURST")
            .env_remove("MAILYPOPPINS_DAEMON_FAKE_OPERATIONS")
            .env_remove("MAILYPOPPINS_DAEMON_FAKE_SYNC_OUTCOME");
        cmd
    }

    /// Spawn `mp daemon run`, killed on drop, and wait until its socket
    /// accepts a connection. Its stdio goes nowhere.
    async fn start_daemon(&self, account_runtimes: bool) -> Proc {
        self.spawn_daemon(account_runtimes, None).await
    }

    /// The same, with `--foreground-logs` and both streams captured, for the
    /// test that reads the daemon's own output back.
    async fn start_daemon_capturing(&self, account_runtimes: bool) -> (Proc, Capture) {
        let capture = Capture {
            stdout: self.root.path().join("daemon.stdout"),
            stderr: self.root.path().join("daemon.stderr"),
        };
        let proc = self.spawn_daemon(account_runtimes, Some(&capture)).await;
        (proc, capture)
    }

    async fn spawn_daemon(&self, account_runtimes: bool, capture: Option<&Capture>) -> Proc {
        let mut cmd = self.cmd();
        if account_runtimes {
            cmd.env(ACCOUNT_RUNTIMES_ENV, "1");
        }
        cmd.args(["daemon", "run"]);
        match capture {
            Some(capture) => {
                cmd.arg("--foreground-logs")
                    .stdout(Stdio::from(
                        fs::File::create(&capture.stdout).expect("stdout capture"),
                    ))
                    .stderr(Stdio::from(
                        fs::File::create(&capture.stderr).expect("stderr capture"),
                    ));
            }
            None => {
                cmd.stdout(Stdio::null()).stderr(Stdio::null());
            }
        }
        let child = cmd
            .stdin(Stdio::null())
            .spawn()
            .expect("spawn mp daemon run");
        let proc = Proc(Some(child));
        self.wait_socket_live().await;
        proc
    }

    async fn wait_socket_live(&self) {
        let start = Instant::now();
        loop {
            if UnixStream::connect(self.socket()).await.is_ok() {
                return;
            }
            assert!(
                start.elapsed() < DEADLINE,
                "the daemon socket {} never accepted a connection within {DEADLINE:?}",
                self.socket().display()
            );
            tokio::time::sleep(TICK).await;
        }
    }

    /// Ask the running daemon to shut down, so its log file is complete before
    /// a test reads it back.
    fn stop_daemon(&self) {
        let out = self
            .cmd()
            .args(["daemon", "stop", "--timeout-secs", "10"])
            .output()
            .expect("run mp daemon stop");
        assert!(
            out.status.success(),
            "mp daemon stop failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// `mp daemon status --json`, parsed.
    fn status_json(&self) -> Value {
        let out = self
            .cmd()
            .args(["daemon", "status", "--json"])
            .output()
            .expect("run mp daemon status --json");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
            panic!("`mp daemon status --json` did not print one JSON object: {e}\nstdout: {stdout}")
        })
    }

    /// The `state` of one account, or `None` when the daemon does not list it.
    fn account_state(&self, account: &str) -> Option<String> {
        let status = self.status_json();
        let accounts = status["accounts"]
            .as_array()
            .unwrap_or_else(|| panic!("accounts is an array, got {status}"))
            .clone();
        accounts
            .iter()
            .find(|entry| entry["name"] == Value::from(account))
            .map(|entry| {
                entry["state"]
                    .as_str()
                    .unwrap_or_else(|| panic!("an account state is a string, got {entry}"))
                    .to_string()
            })
    }

    /// Poll until `account` has left `opening`, and report where it landed.
    fn wait_settled_state(&self, account: &str) -> String {
        let start = Instant::now();
        loop {
            if let Some(state) = self.account_state(account) {
                if state != "opening" {
                    return state;
                }
            }
            assert!(
                start.elapsed() < DEADLINE,
                "{account} never left `opening` within {DEADLINE:?}"
            );
            std::thread::sleep(TICK);
        }
    }

    /// Whether the engine lock for `account` is free right now.
    fn engine_lock_is_free(&self, account: &str) -> bool {
        fs::create_dir_all(self.account_dir(account)).expect("account dir");
        EngineLock::try_acquire_at(&self.lock_path(account), account)
            .expect("the lock file is creatable")
            .is_some()
    }

    /// Everything the daemon wrote to `<data_dir>/logs/`, concatenated.
    fn log_text(&self) -> String {
        let Ok(entries) = fs::read_dir(self.logs_dir()) else {
            return String::new();
        };
        entries
            .flatten()
            .map(|entry| fs::read_to_string(entry.path()).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Ok(raw) = fs::read_to_string(self.pid_file()) {
            if let Ok(pid) = raw.trim().parse::<i32>() {
                if pid > 1 {
                    // Safety: a pid read from a pid file we own.
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
            }
        }
    }
}

/// Where a captured daemon's two streams went.
struct Capture {
    stdout: PathBuf,
    stderr: PathBuf,
}

impl Capture {
    fn text(&self) -> String {
        let read = |path: &Path| fs::read_to_string(path).unwrap_or_default();
        format!("{}\n{}", read(&self.stdout), read(&self.stderr))
    }
}

/// A spawned `mp` process, killed and reaped on drop, so a panicking assertion
/// never leaves a daemon behind.
struct Proc(Option<Child>);

impl Drop for Proc {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

// ---------------------------------------------------------------------------
// Layer (b) - helpers
// ---------------------------------------------------------------------------

/// Await `fut` under [`DEADLINE`], failing the test rather than the suite's
/// patience if the daemon never answers.
async fn within<T>(label: &str, fut: impl Future<Output = T>) -> T {
    match tokio::time::timeout(DEADLINE, fut).await {
        Ok(value) => value,
        Err(_) => panic!("{label} did not finish within {DEADLINE:?}"),
    }
}

fn client_info() -> ClientInfo {
    ClientInfo {
        kind: ClientKind::Gui,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// Connect and complete a well-formed handshake, requiring nothing.
async fn connect_initialized(sandbox: &Sandbox) -> (Connection, InitializeResult) {
    let mut conn = within(
        "Connection::connect",
        Connection::connect(&sandbox.socket()),
    )
    .await
    .expect("connecting to a live daemon socket succeeds");
    let hello = within(
        "Connection::initialize",
        conn.initialize(client_info(), sandbox.identity(), &[], &[]),
    )
    .await
    .expect("a compatible handshake succeeds");
    (conn, hello)
}

/// Call one method and expect a result.
async fn call_ok(conn: &mut Connection, method: &str, params: Value) -> Value {
    within(method, conn.call(method, params))
        .await
        .unwrap_or_else(|e| panic!("{method} was expected to succeed, got {e:?}"))
}

/// Call one method and expect a JSON-RPC error.
async fn call_err(conn: &mut Connection, method: &str, params: Value) -> RpcError {
    let error = within(method, conn.call(method, params))
        .await
        .expect_err("the call was expected to fail");
    match error {
        ClientError::Rpc(error) => error,
        other => {
            panic!("{method} failed for a transport reason rather than a domain one: {other:?}")
        }
    }
}

/// Subscribe this connection to the fan-out, and report the revision the
/// snapshot was captured at.
async fn bootstrap(conn: &mut Connection) -> Value {
    call_ok(conn, "state.bootstrap", json!({})).await
}

/// Read `state.event` notifications until one of kind `config.changed`
/// arrives, and return every event up to and including it.
async fn events_through_config_changed(conn: &mut Connection) -> Vec<EventEnvelope> {
    let mut seen: Vec<EventEnvelope> = Vec::new();
    loop {
        let notification = within("a notification", conn.next_notification())
            .await
            .expect("the daemon delivers a notification rather than closing");
        if notification.method != METHOD_STATE_EVENT {
            continue;
        }
        let envelope: EventEnvelope = serde_json::from_value(notification.params.clone())
            .unwrap_or_else(|e| {
                panic!("the params are an event envelope: {e}; got {notification:?}")
            });
        let done = envelope.kind == KIND_CONFIG_CHANGED;
        seen.push(envelope);
        if done {
            return seen;
        }
    }
}

/// Read notifications until one of kind `config.invalid` arrives, and return
/// its payload, typed.
async fn next_config_invalid(conn: &mut Connection) -> ConfigInvalid {
    loop {
        let notification = within("a notification", conn.next_notification())
            .await
            .expect("the daemon delivers a notification rather than closing");
        if notification.method != METHOD_STATE_EVENT {
            continue;
        }
        let envelope: EventEnvelope = serde_json::from_value(notification.params.clone())
            .unwrap_or_else(|e| {
                panic!("the params are an event envelope: {e}; got {notification:?}")
            });
        if envelope.kind != KIND_CONFIG_INVALID {
            continue;
        }
        return serde_json::from_value::<ConfigInvalid>(envelope.payload.clone()).unwrap_or_else(
            |e| {
                panic!("a config.invalid payload is {{path, line, message}}: {e}; got {envelope:?}")
            },
        );
    }
}

/// One line per event, in arrival order, in the form the ordering assertions
/// read: enough to tell which account each one is about, and nothing else.
fn describe(events: &[EventEnvelope]) -> Vec<String> {
    events
        .iter()
        .map(|event| match event.kind.as_str() {
            "state.remove" => format!(
                "state.remove {}",
                event.payload["resource"].as_str().unwrap_or("?")
            ),
            "account.state_changed" => format!(
                "account.state_changed {} {}",
                event.payload["account"].as_str().unwrap_or("?"),
                event.payload["state"].as_str().unwrap_or("?")
            ),
            other => other.to_string(),
        })
        .collect()
}

/// One account's `[[accounts]]` block, as a user would write it.
fn account_toml(name: &str, body_fetch_deadline_secs: u64) -> String {
    format!(
        "[[accounts]]\n\
         name = \"{name}\"\n\
         default_from = \"{name}@example.com\"\n\
         \n\
         [accounts.imap]\n\
         body_fetch_deadline_secs = {body_fetch_deadline_secs}\n\
         \n\
         [accounts.mailboxes.inbox]\n\
         server = \"INBOX\"\n\
         \n"
    )
}

/// The `account` object `config.init` and `config.add_account` take, which
/// mirrors what the wizard asks for in `src/config_cmd/init.rs`.
fn account_params(name: &str) -> Value {
    json!({
        "name": name,
        "default_from": format!("{name}@example.com"),
        "auth_method": "password",
        "smtp": {
            "host": "smtp.example.com",
            "port": 587,
            "username": format!("{name}@example.com"),
            "accept_invalid_certs": false,
        },
        "imap": {
            "host": "imap.example.com",
            "port": 143,
            "username": format!("{name}@example.com"),
            "accept_invalid_certs": false,
            "body_fetch_deadline_secs": 12,
        },
        "mailboxes": {
            "inbox": "INBOX",
            "archive": "Archive",
            "sent": "Sent",
            "extra": ["Newsletters"],
        },
        "save_to_sent": "auto",
    })
}

fn sorted_keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .unwrap_or_else(|| panic!("expected a JSON object, got {value}"))
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

fn assert_keys(value: &Value, expected: &[&str], what: &str) {
    assert_eq!(
        sorted_keys(value),
        expected.iter().map(|k| k.to_string()).collect::<Vec<_>>(),
        "{what} has exactly the documented keys, got {value}"
    );
}

/// Every string anywhere in a JSON value, so a secrecy assertion can walk a
/// whole result rather than the fields it remembered to name.
fn string_values(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| string_values(item, out)),
        Value::Object(map) => map.values().for_each(|item| string_values(item, out)),
        _ => {}
    }
}

/// The account named `name` in a `config.get` result.
fn account_of<'a>(get: &'a Value, name: &str) -> &'a Value {
    get["config"]["accounts"]
        .as_array()
        .unwrap_or_else(|| panic!("accounts is an array, got {get}"))
        .iter()
        .find(|account| account["name"] == Value::from(name))
        .unwrap_or_else(|| panic!("{name} is configured, got {get}"))
}

/// The names of every configured account, in the order `config.get` reports
/// them, which is the order they appear in `config.toml`.
fn account_names(get: &Value) -> Vec<String> {
    get["config"]["accounts"]
        .as_array()
        .unwrap_or_else(|| panic!("accounts is an array, got {get}"))
        .iter()
        .map(|account| account["name"].as_str().unwrap_or("?").to_string())
        .collect()
}

/// The 1-based line `needle` sits on in `document`.
fn line_of(document: &str, needle: &str) -> u32 {
    let at = document
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} is in the document"));
    (document[..at].matches('\n').count() + 1) as u32
}

/// A candidate document that parses as TOML nowhere near its end, so the span
/// a parser reports is unambiguous.
fn syntactically_broken_document() -> String {
    format!(
        "{}{BROKEN_LINE}\n",
        account_toml("alpha", DEFAULT_BODY_FETCH_DEADLINE_SECS)
    )
}

/// A candidate document that is valid TOML and refuses to load: the horizon is
/// far past `MAX_HORIZON_DAYS`, which is what `validate_retention` rejects.
fn semantically_broken_document() -> String {
    format!(
        "[retention]\nmetadata_horizon_days = 40000\n\n{}",
        account_toml("alpha", DEFAULT_BODY_FETCH_DEADLINE_SECS)
    )
}

// ---------------------------------------------------------------------------
// Layer (b) - the six methods are served and advertised
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_capability_list_is_exactly_the_declared_config_family() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let _daemon = sandbox.start_daemon(false).await;
    let (_conn, hello) = connect_initialized(&sandbox).await;

    let mut served: Vec<&str> = hello
        .capabilities
        .iter()
        .map(String::as_str)
        .filter(|name| name.starts_with("config."))
        .collect();
    served.sort();
    assert_eq!(
        served,
        CONFIG_METHODS.to_vec(),
        "the handshake advertises the six methods CONFIG_METHOD_SPECS declares, and no other \
         member of the config family: the capability list is derived from the dispatcher's \
         table, so this is what is actually served"
    );
}

// ---------------------------------------------------------------------------
// Layer (b) - config.get
// ---------------------------------------------------------------------------

#[tokio::test]
async fn config_get_reports_the_effective_configuration_with_every_secret_redacted() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let _daemon = sandbox.start_daemon(false).await;
    let (mut conn, _hello) = connect_initialized(&sandbox).await;

    let get = call_ok(&mut conn, "config.get", json!({})).await;
    assert_keys(&get, &GET_KEYS, "a config.get result");
    assert_eq!(
        get["revision"],
        json!(0),
        "the configuration the daemon started with is revision 0"
    );
    assert_eq!(
        get["path"],
        json!(sandbox.config_path().display().to_string()),
        "the path is the file the daemon read, spelled as `initialize` spells it"
    );
    assert_eq!(get["state"], json!("ok"));

    let config = &get["config"];
    assert_keys(config, &CONFIG_KEYS, "the effective config");
    assert_eq!(
        config["theme"],
        json!(""),
        "an unset theme is the empty name"
    );
    assert_eq!(config["notifications"], json!(false));
    assert_eq!(
        config["secrets_backend"],
        json!("encrypted-file"),
        "the default backend, spelled as `SecretsBackendKind` spells it on the wire"
    );

    assert_keys(&config["email"], &EMAIL_KEYS, "the email settings");
    assert_eq!(
        config["email"]["send_hold_secs"],
        json!(DEFAULT_SEND_HOLD_SECS),
        "an omitted field reports the default the loader applied, not nothing"
    );
    assert_eq!(config["email"]["include_signature"], json!(true));

    let defaults = RetentionPolicy::default();
    for retention in [
        &config["retention"],
        &account_of(&get, "alpha")["retention"],
    ] {
        assert_keys(retention, &RETENTION_KEYS, "a resolved retention policy");
        assert_eq!(
            retention["metadata_horizon_days"],
            json!(defaults.metadata_horizon_days)
        );
        assert_eq!(
            retention["body_horizon_days"],
            json!(defaults.body_horizon_days)
        );
        assert_eq!(
            retention["attachment_horizon_days"],
            json!(defaults.attachment_horizon_days)
        );
        assert_eq!(retention["max_disk_bytes"], json!(defaults.max_disk_bytes));
    }

    let alpha = account_of(&get, "alpha");
    assert_keys(alpha, &ACCOUNT_KEYS, "one account");
    assert_eq!(alpha["default_from"], json!("alpha@example.com"));
    assert_eq!(alpha["auth_method"], json!("password"));
    assert_eq!(alpha["save_to_sent"], json!("auto"));
    assert_eq!(
        alpha["oauth2"],
        Value::Null,
        "an account with no [accounts.oauth2] table reports null rather than an empty object"
    );

    assert_keys(&alpha["smtp"], &SMTP_KEYS, "an account's smtp settings");
    assert_eq!(alpha["smtp"]["host"], json!(""));
    assert_eq!(
        alpha["smtp"]["port"],
        json!(DEFAULT_SMTP_PORT),
        "effective, so the omitted port is the default"
    );
    assert_eq!(alpha["smtp"]["accept_invalid_certs"], json!(false));

    assert_keys(&alpha["imap"], &IMAP_KEYS, "an account's imap settings");
    assert_eq!(alpha["imap"]["port"], json!(DEFAULT_IMAP_PORT));
    assert_eq!(
        alpha["imap"]["fetch_concurrency"],
        json!(DEFAULT_FETCH_CONCURRENCY)
    );
    assert_eq!(
        alpha["imap"]["body_fetch_deadline_secs"],
        json!(DEFAULT_BODY_FETCH_DEADLINE_SECS)
    );

    assert_eq!(
        alpha["smtp"]["password"],
        json!(REDACTED),
        "the SMTP password field is present and carries the literal, never a value and never a \
         length"
    );
    assert_eq!(alpha["imap"]["password"], json!(REDACTED));

    assert_keys(
        &alpha["mailboxes"],
        &MAILBOX_KEYS,
        "an account's mailbox mappings",
    );
    assert_eq!(alpha["mailboxes"]["inbox"], json!({"server": "INBOX"}));
    assert_eq!(
        alpha["mailboxes"]["archive"],
        Value::Null,
        "an unmapped role is null"
    );
    assert_eq!(
        alpha["mailboxes"]["extra"],
        json!([]),
        "and an absent extra list is empty rather than null"
    );
}

// ---------------------------------------------------------------------------
// Layer (b) - config.validate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn config_validate_accepts_a_good_document_and_changes_nothing() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let _daemon = sandbox.start_daemon(false).await;
    let (mut conn, _hello) = connect_initialized(&sandbox).await;

    let candidate = format!(
        "{}{}",
        account_toml("alpha", 7),
        account_toml("delta", DEFAULT_BODY_FETCH_DEADLINE_SECS)
    );
    let result = call_ok(&mut conn, "config.validate", json!({"toml": candidate})).await;
    assert_eq!(result, json!({"ok": true}));
    assert_keys(&result, &["ok"], "an accepted candidate");

    let get = call_ok(&mut conn, "config.get", json!({})).await;
    assert_eq!(
        account_names(&get),
        vec!["alpha".to_string()],
        "validating a two-account candidate did not adopt it"
    );
    assert_eq!(
        get["revision"],
        json!(0),
        "and it moved no configuration revision"
    );
    assert_eq!(
        sandbox.read_config(),
        account_toml("alpha", DEFAULT_BODY_FETCH_DEADLINE_SECS),
        "nor did it write anything to disk"
    );
}

#[tokio::test]
async fn config_validate_reports_the_line_of_a_syntax_error() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let _daemon = sandbox.start_daemon(false).await;
    let (mut conn, _hello) = connect_initialized(&sandbox).await;

    let candidate = syntactically_broken_document();
    let result = call_ok(&mut conn, "config.validate", json!({"toml": candidate})).await;
    assert_keys(&result, &["errors", "ok"], "a refused candidate");
    assert_eq!(result["ok"], json!(false));

    let errors = result["errors"]
        .as_array()
        .unwrap_or_else(|| panic!("errors is an array, got {result}"));
    assert!(
        !errors.is_empty(),
        "a refusal names at least one thing that is wrong"
    );
    let first = &errors[0];
    assert_keys(first, &["line", "message"], "one diagnostic");
    assert_eq!(
        first["line"],
        json!(line_of(&candidate, BROKEN_LINE)),
        "the 1-based line of the offending token, counted from the span the TOML parser reports"
    );
    assert!(
        first["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "and a message a user can act on, got {first}"
    );
}

#[tokio::test]
async fn config_validate_reports_a_semantic_refusal_with_no_line() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let _daemon = sandbox.start_daemon(false).await;
    let (mut conn, _hello) = connect_initialized(&sandbox).await;

    let result = call_ok(
        &mut conn,
        "config.validate",
        json!({"toml": semantically_broken_document()}),
    )
    .await;
    assert_eq!(result["ok"], json!(false));
    let first = &result["errors"][0];
    assert_eq!(
        first["line"],
        Value::Null,
        "a document that parses and does not load has no offending token to point at, got {first}"
    );
    assert!(
        first["message"]
            .as_str()
            .is_some_and(|message| message.contains("metadata_horizon_days")),
        "the message names the setting that is out of range, got {first}"
    );
}

#[tokio::test]
async fn config_validate_needs_its_toml_parameter() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let _daemon = sandbox.start_daemon(false).await;
    let (mut conn, _hello) = connect_initialized(&sandbox).await;

    let error = call_err(&mut conn, "config.validate", json!({})).await;
    assert_eq!(
        error.code, -32602,
        "a missing required parameter is invalid_params, got {error:?}"
    );
}

// ---------------------------------------------------------------------------
// Layer (b) - config.reload
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_valid_swap_stops_removed_updates_changed_and_starts_added_in_that_order() {
    let sandbox = Sandbox::with_accounts(&["alpha", "beta"]);
    let _daemon = sandbox.start_daemon(true).await;
    // Both runtimes settle before the connection subscribes, so every event
    // this test collects belongs to the reload and nothing races it.
    assert_eq!(sandbox.wait_settled_state("alpha"), "ready");
    assert_eq!(sandbox.wait_settled_state("beta"), "ready");

    let (mut conn, _hello) = connect_initialized(&sandbox).await;
    bootstrap(&mut conn).await;

    // alpha goes, beta's body-fetch budget changes, gamma appears - and
    // gamma's data directory deliberately does not exist yet.
    sandbox.write_config(&format!(
        "{}{}",
        account_toml("beta", 11),
        account_toml("gamma", DEFAULT_BODY_FETCH_DEADLINE_SECS)
    ));
    assert!(
        !sandbox.account_dir("gamma").exists(),
        "the test has not created gamma's directory; the daemon must"
    );

    // The reload runs on a connection of its own so this one can read the
    // events *while* the swap is in flight: what the order of the envelopes
    // cannot say is whether the removed runtime was actually gone by the time
    // the added one came up (#0122 review). The lock is sampled the moment
    // gamma's readiness is decoded, which is strictly after the daemon
    // published it, so a swap that started gamma before dropping alpha is
    // caught with alpha's lock still held.
    let mut driver = connect_initialized(&sandbox).await.0;
    let reload = tokio::spawn(async move {
        let result = call_ok(&mut driver, "config.reload", json!({})).await;
        (driver, result)
    });

    let mut events: Vec<EventEnvelope> = Vec::new();
    let mut alpha_lock_at_gamma_ready: Option<bool> = None;
    loop {
        let notification = within("a notification", conn.next_notification())
            .await
            .expect("the daemon delivers a notification rather than closing");
        if notification.method != METHOD_STATE_EVENT {
            continue;
        }
        let envelope: EventEnvelope = serde_json::from_value(notification.params.clone())
            .unwrap_or_else(|e| {
                panic!("the params are an event envelope: {e}; got {notification:?}")
            });
        if envelope.kind == "account.state_changed"
            && envelope.payload["account"] == json!("gamma")
            && envelope.payload["state"] == json!("ready")
        {
            alpha_lock_at_gamma_ready = Some(sandbox.engine_lock_is_free("alpha"));
        }
        let done = envelope.kind == KIND_CONFIG_CHANGED;
        events.push(envelope);
        if done {
            break;
        }
    }

    let (_driver, result) = reload.await.expect("the reload task finished");
    assert_keys(&result, &RECONCILE_KEYS, "a config.reload result");
    assert_eq!(result["removed"], json!(["alpha"]));
    assert_eq!(
        result["updated"],
        json!(["beta"]),
        "beta's effective configuration changed, so it counts as updated"
    );
    assert_eq!(result["added"], json!(["gamma"]));

    assert_eq!(
        alpha_lock_at_gamma_ready,
        Some(true),
        "the removed account's engine lock is free by the time the added account reports ready: \
         a stop that only ordered its event ahead would leave the next engine locked out"
    );
    assert_eq!(
        describe(&events),
        vec![
            "state.remove account:alpha".to_string(),
            "account.state_changed beta ready".to_string(),
            "account.state_changed gamma ready".to_string(),
            "config.changed".to_string(),
        ],
        "the reconciliation is stop-removed, then update, then start-added, and the announcement \
         comes last: everything the swap did is in front of the event that announces it"
    );

    let mut revisions: Vec<u64> = events.iter().map(|event| event.revision).collect();
    let sorted = {
        let mut copy = revisions.clone();
        copy.sort_unstable();
        copy
    };
    assert_eq!(revisions, sorted, "and it arrives in revision order");
    revisions.dedup();
    assert_eq!(
        revisions.len(),
        events.len(),
        "each step of the swap commits its own revision"
    );

    let changed: ConfigChanged =
        serde_json::from_value(events.last().expect("at least one event").payload.clone())
            .expect("a config.changed payload is the three lists and a revision");
    assert_eq!(
        changed,
        ConfigChanged {
            added: vec!["gamma".to_string()],
            updated: vec!["beta".to_string()],
            removed: vec!["alpha".to_string()],
            config_revision: 1,
        },
        "the event carries what the call returned, plus the revision the swap moved to"
    );

    let get = call_ok(&mut conn, "config.get", json!({})).await;
    assert_eq!(get["revision"], json!(1), "and config.get agrees with it");
    assert_eq!(
        account_names(&get),
        vec!["beta".to_string(), "gamma".to_string()],
        "the new snapshot is live"
    );
    assert_eq!(
        account_of(&get, "beta")["imap"]["body_fetch_deadline_secs"],
        json!(11),
        "with beta's new budget"
    );

    assert!(
        sandbox.account_dir("gamma").exists(),
        "a started runtime's account directory is created when it is missing"
    );
    assert_eq!(sandbox.account_state("beta").as_deref(), Some("ready"));
    assert_eq!(sandbox.account_state("gamma").as_deref(), Some("ready"));
    assert_eq!(
        sandbox.account_state("alpha"),
        None,
        "a removed account is not reported at all"
    );
}

#[tokio::test]
async fn removing_an_account_stops_its_runtime_and_releases_its_engine_lock() {
    let sandbox = Sandbox::with_accounts(&["alpha", "beta"]);
    let _daemon = sandbox.start_daemon(true).await;
    assert_eq!(sandbox.wait_settled_state("alpha"), "ready");
    assert_eq!(sandbox.wait_settled_state("beta"), "ready");
    assert!(
        !sandbox.engine_lock_is_free("alpha"),
        "a live runtime holds its account's engine lock"
    );

    let (mut conn, _hello) = connect_initialized(&sandbox).await;
    sandbox.write_config(&account_toml("beta", DEFAULT_BODY_FETCH_DEADLINE_SECS));

    let result = call_ok(&mut conn, "config.reload", json!({})).await;
    assert_eq!(result["removed"], json!(["alpha"]));

    // No poll: the reload answers only once every runtime it touched has
    // settled, so the lock is free the moment the call returns.
    assert!(
        sandbox.engine_lock_is_free("alpha"),
        "dropping a removed account's runtime releases its engine lock for the next engine"
    );
    assert!(
        !sandbox.engine_lock_is_free("beta"),
        "and the account that stayed still holds its own"
    );
}

#[tokio::test]
async fn an_invalid_edit_keeps_the_previous_snapshot_live_and_publishes_a_diagnostic() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let _daemon = sandbox.start_daemon(true).await;
    assert_eq!(sandbox.wait_settled_state("alpha"), "ready");

    let (mut conn, _hello) = connect_initialized(&sandbox).await;
    bootstrap(&mut conn).await;

    let broken = syntactically_broken_document();
    sandbox.write_config(&broken);

    let error = call_err(&mut conn, "config.reload", json!({})).await;
    assert_eq!(
        error.code,
        ErrorCode::ConfigInvalid.code(),
        "a candidate that does not load is -32007, got {error:?}"
    );
    let data = error
        .data
        .clone()
        .expect("config_invalid carries a payload");
    assert_keys(&data, &["line", "message", "path"], "the error payload");
    assert_eq!(
        data["path"],
        json!(sandbox.config_path().display().to_string())
    );
    assert_eq!(data["line"], json!(line_of(&broken, BROKEN_LINE)));

    let event = next_config_invalid(&mut conn).await;
    assert_eq!(
        event,
        ConfigInvalid {
            path: sandbox.config_path().display().to_string(),
            line: Some(line_of(&broken, BROKEN_LINE)),
            message: data["message"].as_str().unwrap_or_default().to_string(),
        },
        "the event carries the same three fields the refusal did, so a watcher and a caller \
         render one diagnostic"
    );

    // The previous snapshot is untouched: the accounts, the revision, the
    // runtime and the lock it holds.
    let get = call_ok(&mut conn, "config.get", json!({})).await;
    assert_eq!(account_names(&get), vec!["alpha".to_string()]);
    assert_eq!(get["state"], json!("ok"));
    assert_eq!(
        get["revision"],
        json!(0),
        "a rejected candidate moves no configuration revision"
    );
    assert_eq!(
        sandbox.account_state("alpha").as_deref(),
        Some("ready"),
        "the runtime that was serving is still serving"
    );
    assert!(
        !sandbox.engine_lock_is_free("alpha"),
        "and it still holds its engine lock"
    );
}

#[tokio::test]
async fn a_reload_that_changes_nothing_still_announces_itself() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let _daemon = sandbox.start_daemon(false).await;
    let (mut conn, _hello) = connect_initialized(&sandbox).await;
    bootstrap(&mut conn).await;

    let result = call_ok(&mut conn, "config.reload", json!({})).await;
    assert_eq!(
        result,
        json!({"added": [], "updated": [], "removed": []}),
        "nothing on disk changed, so nothing was reconciled"
    );

    let first = events_through_config_changed(&mut conn).await;
    assert_eq!(describe(&first), vec!["config.changed".to_string()]);
    let first: ConfigChanged =
        serde_json::from_value(first.last().expect("one event").payload.clone())
            .expect("a config.changed payload");
    assert_eq!(first.config_revision, 1, "a swap happened, empty or not");

    call_ok(&mut conn, "config.reload", json!({})).await;
    let second = events_through_config_changed(&mut conn).await;
    let second: ConfigChanged =
        serde_json::from_value(second.last().expect("one event").payload.clone())
            .expect("a config.changed payload");
    assert_eq!(
        second.config_revision, 2,
        "two swaps are two events with two revisions: a lifecycle event never coalesces, so a \
         slow client cannot be shown the second swap's lists in place of the first's"
    );
}

// ---------------------------------------------------------------------------
// Layer (b) - the first run
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_daemon_with_no_config_serves_the_config_family_and_reports_zero_accounts() {
    let sandbox = Sandbox::empty();
    assert!(!sandbox.config_path().exists());
    let _daemon = sandbox.start_daemon(true).await;

    let status = sandbox.status_json();
    assert_eq!(status["running"], json!(true));
    assert_eq!(
        status["accounts"],
        json!([]),
        "no configuration is zero accounts, not a startup failure"
    );

    let (mut conn, hello) = connect_initialized(&sandbox).await;
    for name in CONFIG_METHODS {
        assert!(
            hello.capabilities.iter().any(|cap| cap == name),
            "{name} is served by a daemon that has no configuration yet: it is how one gets \
             written"
        );
    }

    let get = call_ok(&mut conn, "config.get", json!({})).await;
    assert_keys(
        &get,
        &GET_KEYS,
        "a config.get result with no file behind it",
    );
    assert_eq!(get["state"], json!("absent"));
    assert_eq!(
        get["path"],
        json!(sandbox.config_path().display().to_string()),
        "the path the daemon looked at, so a client can tell the user where to write"
    );
    assert_eq!(get["revision"], json!(0));
    assert_eq!(get["config"]["accounts"], json!([]));
    assert_keys(&get["config"], &CONFIG_KEYS, "the empty effective config");

    let error = call_err(
        &mut conn,
        "config.add_account",
        json!({"account": account_params("alpha")}),
    )
    .await;
    assert_eq!(
        error.code, -32602,
        "adding to a file that does not exist is config.init's job, got {error:?}"
    );
    assert!(
        error.message.contains("config.init"),
        "and the refusal names the method that does it: {}",
        error.message
    );
}

#[tokio::test]
async fn config_init_writes_a_config_the_daemon_then_loads_and_runs() {
    let sandbox = Sandbox::empty();
    let _daemon = sandbox.start_daemon(true).await;
    let (mut conn, _hello) = connect_initialized(&sandbox).await;
    bootstrap(&mut conn).await;

    let result = call_ok(
        &mut conn,
        "config.init",
        json!({
            "account": account_params("alpha"),
            "secrets_backend": "encrypted-file",
            "theme": "tokyo-night",
            "notifications": true,
        }),
    )
    .await;
    assert_keys(
        &result,
        &["added", "path", "removed", "updated"],
        "a config.init result",
    );
    assert_eq!(result["added"], json!(["alpha"]));
    assert_eq!(result["updated"], json!([]));
    assert_eq!(result["removed"], json!([]));
    assert_eq!(
        result["path"],
        json!(sandbox.config_path().display().to_string())
    );
    assert!(
        sandbox.config_path().exists(),
        "config.init wrote the file it named"
    );

    let events = events_through_config_changed(&mut conn).await;
    assert_eq!(
        describe(&events),
        vec![
            "account.state_changed alpha ready".to_string(),
            "config.changed".to_string(),
        ],
        "a written configuration is reconciled like any other swap, and announced last"
    );

    let get = call_ok(&mut conn, "config.get", json!({})).await;
    assert_eq!(get["state"], json!("ok"), "the daemon loaded what it wrote");
    assert_eq!(get["revision"], json!(1));
    assert_eq!(account_names(&get), vec!["alpha".to_string()]);
    assert_eq!(get["config"]["theme"], json!("tokyo-night"));
    assert_eq!(get["config"]["notifications"], json!(true));

    let alpha = account_of(&get, "alpha");
    assert_eq!(alpha["smtp"]["host"], json!("smtp.example.com"));
    assert_eq!(alpha["smtp"]["port"], json!(587));
    assert_eq!(alpha["imap"]["host"], json!("imap.example.com"));
    assert_eq!(alpha["imap"]["port"], json!(143));
    assert_eq!(alpha["imap"]["body_fetch_deadline_secs"], json!(12));
    assert_eq!(alpha["mailboxes"]["inbox"], json!({"server": "INBOX"}));
    assert_eq!(alpha["mailboxes"]["archive"], json!({"server": "Archive"}));
    assert_eq!(alpha["mailboxes"]["sent"], json!({"server": "Sent"}));
    assert_eq!(
        alpha["mailboxes"]["extra"],
        json!([{"server": "Newsletters"}])
    );
    assert_eq!(alpha["smtp"]["password"], json!(REDACTED));

    // What it wrote is a document it would accept again, which is the whole
    // claim behind "the daemon then loads it".
    let written = sandbox.read_config();
    let revalidated = call_ok(&mut conn, "config.validate", json!({"toml": written})).await;
    assert_eq!(revalidated, json!({"ok": true}));

    assert_eq!(
        sandbox.account_state("alpha").as_deref(),
        Some("ready"),
        "and the account it configured is running"
    );
    assert!(
        sandbox.account_dir("alpha").exists(),
        "config.init creates the account's data directory, as the wizard it mirrors does"
    );
    assert!(!sandbox.engine_lock_is_free("alpha"));
}

#[tokio::test]
async fn config_init_refuses_to_overwrite_an_existing_config() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let before = sandbox.read_config();
    let _daemon = sandbox.start_daemon(false).await;
    let (mut conn, _hello) = connect_initialized(&sandbox).await;

    let error = call_err(
        &mut conn,
        "config.init",
        json!({"account": account_params("delta")}),
    )
    .await;
    assert_eq!(
        error.code, -32602,
        "the wizard asks 'Overwrite? [y/N]' and a daemon has nobody to ask, got {error:?}"
    );
    assert!(
        error
            .message
            .contains(&sandbox.config_path().display().to_string()),
        "the refusal names the file that is in the way: {}",
        error.message
    );
    assert_eq!(
        sandbox.read_config(),
        before,
        "and leaves it byte-identical"
    );
}

// ---------------------------------------------------------------------------
// Layer (b) - config.add_account
// ---------------------------------------------------------------------------

#[tokio::test]
async fn config_add_account_appends_an_account_and_reconciles_it() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let _daemon = sandbox.start_daemon(true).await;
    assert_eq!(sandbox.wait_settled_state("alpha"), "ready");

    let (mut conn, _hello) = connect_initialized(&sandbox).await;
    bootstrap(&mut conn).await;

    let result = call_ok(
        &mut conn,
        "config.add_account",
        json!({"account": account_params("delta")}),
    )
    .await;
    assert_keys(&result, &RECONCILE_KEYS, "a config.add_account result");
    assert_eq!(result["added"], json!(["delta"]));
    assert_eq!(
        result["updated"],
        json!([]),
        "an append leaves every other account's effective configuration alone"
    );
    assert_eq!(result["removed"], json!([]));

    let events = events_through_config_changed(&mut conn).await;
    assert_eq!(
        describe(&events),
        vec![
            "account.state_changed delta ready".to_string(),
            "config.changed".to_string(),
        ]
    );

    let get = call_ok(&mut conn, "config.get", json!({})).await;
    assert_eq!(
        account_names(&get),
        vec!["alpha".to_string(), "delta".to_string()],
        "the new account is appended, so the order a user reads is the order they wrote"
    );
    assert_eq!(get["revision"], json!(1));
    assert_eq!(
        account_of(&get, "alpha")["imap"]["body_fetch_deadline_secs"],
        json!(DEFAULT_BODY_FETCH_DEADLINE_SECS),
        "and the account that was already there is untouched"
    );
    assert!(!sandbox.engine_lock_is_free("delta"));
}

#[tokio::test]
async fn config_add_account_refuses_a_duplicate_name_and_leaves_the_file_untouched() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let before = sandbox.read_config();
    let _daemon = sandbox.start_daemon(false).await;
    let (mut conn, _hello) = connect_initialized(&sandbox).await;

    let error = call_err(
        &mut conn,
        "config.add_account",
        json!({"account": account_params("alpha")}),
    )
    .await;
    assert_eq!(
        error.code, -32602,
        "a name that is already taken is a caller error, got {error:?}"
    );
    assert!(
        error.message.contains("alpha"),
        "and the refusal names it: {}",
        error.message
    );
    assert_eq!(sandbox.read_config(), before);

    let get = call_ok(&mut conn, "config.get", json!({})).await;
    assert_eq!(account_names(&get), vec!["alpha".to_string()]);
    assert_eq!(get["revision"], json!(0), "nothing was swapped");
}

// ---------------------------------------------------------------------------
// Layer (b) - config.set_password, and the secret that never escapes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn config_set_password_stores_through_the_existing_backend() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let _daemon = sandbox.start_daemon(false).await;
    let (mut conn, _hello) = connect_initialized(&sandbox).await;

    let result = call_ok(
        &mut conn,
        "config.set_password",
        json!({"account": "alpha", "kind": "smtp", "value": SECRET}),
    )
    .await;
    assert_keys(
        &result,
        &["account", "key", "kind", "stored"],
        "a config.set_password result",
    );
    assert_eq!(result["stored"], json!(true));
    assert_eq!(result["account"], json!("alpha"));
    assert_eq!(result["kind"], json!("smtp"));
    assert_eq!(
        result["key"],
        json!(SecretKind::Smtp.secret_key("alpha")),
        "the key the pre-daemon binary already reads, so the two agree about where a password is"
    );

    assert!(
        sandbox.secrets_path().exists(),
        "the value went into the secrets backend the config selects, not into config.toml"
    );
    assert!(
        !sandbox.read_config().contains(SECRET),
        "and never into config.toml"
    );

    let imap = call_ok(
        &mut conn,
        "config.set_password",
        json!({"account": "alpha", "kind": "imap", "value": SECRET}),
    )
    .await;
    assert_eq!(imap["key"], json!(SecretKind::Imap.secret_key("alpha")));
}

#[tokio::test]
async fn a_secret_never_reaches_a_log_line_an_error_payload_or_the_effective_config() {
    let sandbox = Sandbox::with_accounts(&["alpha"]);
    let (daemon, capture) = sandbox.start_daemon_capturing(true).await;
    assert_eq!(sandbox.wait_settled_state("alpha"), "ready");

    let (mut conn, _hello) = connect_initialized(&sandbox).await;

    // The write that succeeds.
    let stored = call_ok(
        &mut conn,
        "config.set_password",
        json!({"account": "alpha", "kind": "smtp", "value": SECRET}),
    )
    .await;
    assert_no_secret(&stored.to_string(), "the config.set_password result");

    // The write that fails on an unknown account, which is the error payload
    // the plan calls out by name.
    let unknown = call_err(
        &mut conn,
        "config.set_password",
        json!({"account": "nope", "kind": "imap", "value": SECRET}),
    )
    .await;
    assert_eq!(
        unknown.code,
        ErrorCode::AccountUnknown.code(),
        "an account nothing configures is -32005, got {unknown:?}"
    );
    assert_eq!(
        unknown.data,
        Some(json!({"account": "nope"})),
        "the payload the daemon table fixes for that code, and nothing more"
    );
    assert_no_secret(&unknown.message, "the account_unknown message");
    assert_no_secret(
        &unknown.data.clone().unwrap_or(Value::Null).to_string(),
        "the account_unknown payload",
    );

    // The write that fails on the kind, the other place a rejected value is
    // tempting to quote back.
    let bad_kind = call_err(
        &mut conn,
        "config.set_password",
        json!({"account": "alpha", "kind": "pop3", "value": SECRET}),
    )
    .await;
    assert_eq!(bad_kind.code, -32602);
    assert_no_secret(&bad_kind.message, "the invalid-kind message");
    assert_no_secret(
        &bad_kind.data.clone().unwrap_or(Value::Null).to_string(),
        "the invalid-kind payload",
    );

    // The effective configuration, walked value by value rather than field by
    // remembered field.
    let get = call_ok(&mut conn, "config.get", json!({})).await;
    let mut strings = Vec::new();
    string_values(&get, &mut strings);
    for value in &strings {
        assert!(
            !value.contains(SECRET),
            "a config.get result carried the password: {value}"
        );
    }
    assert_eq!(
        account_of(&get, "alpha")["smtp"]["password"],
        json!(REDACTED)
    );

    // A clean shutdown, so the log file is complete before it is read.
    drop(conn);
    sandbox.stop_daemon();
    drop(daemon);

    let logs = sandbox.log_text();
    assert!(
        !logs.is_empty(),
        "the daemon logged something, so this assertion is about a file with content in it: {}",
        sandbox.logs_dir().display()
    );
    assert_no_secret(&logs, "the daemon log file");
    assert_no_secret(&capture.text(), "the daemon's own stdout and stderr");

    let secrets = fs::read(sandbox.secrets_path()).expect("the secrets file was written");
    assert!(
        !String::from_utf8_lossy(&secrets).contains(SECRET),
        "the secrets file holds the password encrypted, never in the clear"
    );
}

/// Fail with a message that names the haystack, and never prints it: a
/// secrecy assertion that dumped the log on failure would put the secret in
/// the test output it was written to keep it out of.
fn assert_no_secret(haystack: &str, what: &str) {
    if let Some(at) = haystack.find(SECRET) {
        let line = haystack[..at].matches('\n').count() + 1;
        panic!("{what} carries the password, at line {line}");
    }
}
