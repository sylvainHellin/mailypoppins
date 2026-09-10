//! The `config.*` family: read, validate, swap, and the one path into the
//! secrets backend (P3b-U8).
//!
//! Six methods, declared once in [`CONFIG_METHOD_SPECS`] and registered from
//! it, so a method cannot be served under a name the array does not carry.
//! Every one of them is [`CancelScope::Durable`]: a configuration swap that
//! undid itself because the client that asked for it exited would leave the
//! daemon serving a configuration nobody chose.
//!
//! - `config.get` reports the effective configuration, wrapped in the revision
//!   and the path it was read from, with `smtp.password` and `imap.password`
//!   always present and always [`REDACTED`].
//! - `config.validate` is a pure function of the string it is given: it reads
//!   no file, writes no file and moves no revision.
//! - `config.reload` re-reads `config.toml` and swaps it, or refuses with
//!   `-32007` and leaves the previous snapshot live.
//! - `config.set_password` writes `smtp-password-<account>` /
//!   `imap-password-<account>` through the backend the pre-daemon binary
//!   already reads, publishes no event, and never lets the value reach a log
//!   line, an error payload or `config.get`.
//! - `config.add_account` appends one `[[accounts]]` block; `config.init`
//!   writes a whole file. Both validate the candidate document *before* they
//!   write it, so a refusal leaves the file byte-identical, and both carry no
//!   secret: the daemon-era equivalent of the wizard's password prompt is a
//!   second call to `config.set_password`, and one path into the secrets
//!   backend is one path to audit.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use futures::future::BoxFuture;
use log::info;
use serde_json::{json, Value};

use mp_protocol::events::{ConfigChanged, ConfigInvalid, KIND_CONFIG_CHANGED, KIND_CONFIG_INVALID};
use mp_protocol::ErrorCode;

use crate::config::GlobalConfig;
use crate::daemon::config::{
    reconcile, validate_document, ConfigState, ConfigStore, Diagnostic, Reconcile,
};
use crate::daemon::server::RuntimeTable;
use crate::daemon::state::events::Event;
use crate::daemon::state::CanonicalState;

use super::super::dispatch::{
    CancelToken, ClientCtx, Dispatcher, DomainError, Method, MethodKind, MethodSpec, Outcome,
    ResourceId,
};

/// What every redacted field carries, whatever it hides.
///
/// One fixed literal: a length, a prefix or a fixed number of asterisks all
/// leak something about the value they stand in for.
pub const REDACTED: &str = "<redacted>";

/// The six methods this family serves, in the name order the dispatcher's table
/// keeps.
pub const CONFIG_METHOD_SPECS: [MethodSpec; 6] = [
    MethodSpec::new("config.add_account", MethodKind::Command, 1),
    MethodSpec::new("config.get", MethodKind::Query, 1),
    MethodSpec::new("config.init", MethodKind::Command, 1),
    MethodSpec::new("config.reload", MethodKind::Command, 1),
    MethodSpec::new("config.set_password", MethodKind::Command, 1),
    MethodSpec::new("config.validate", MethodKind::Query, 1),
];

/// Which credential a secret belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretKind {
    /// The SMTP password.
    Smtp,
    /// The IMAP password, which `ImapConfig::load` reads before falling back
    /// to the SMTP one.
    Imap,
}

impl SecretKind {
    /// The word this kind travels as.
    pub fn as_str(self) -> &'static str {
        match self {
            SecretKind::Smtp => "smtp",
            SecretKind::Imap => "imap",
        }
    }

    /// The kind a wire word names, or `None` for anything else. One spelling,
    /// as everywhere else in the protocol.
    pub fn from_wire(value: &str) -> Option<SecretKind> {
        match value {
            "smtp" => Some(SecretKind::Smtp),
            "imap" => Some(SecretKind::Imap),
            _ => None,
        }
    }

    /// The backend key this kind is stored under, which is the one
    /// `src/config_cmd/init.rs` and `SmtpConfig::load` already use: a daemon
    /// that invented its own namespace would store a password the pre-daemon
    /// binary cannot find.
    pub fn secret_key(self, account: &str) -> String {
        format!("{}-password-{account}", self.as_str())
    }
}

/// The parameters of `config.set_password`.
///
/// `Debug` is hand-written and prints [`REDACTED`] where the value is. A
/// `#[derive(Debug)]` here is the failure this guards against, and it is worth
/// guarding precisely because it is invisible until the day someone writes
/// `debug!("{params:?}")`.
#[derive(Clone)]
pub struct SetPasswordParams {
    account: String,
    kind: SecretKind,
    value: String,
}

impl SetPasswordParams {
    /// The parameters of one write.
    pub fn new(account: impl Into<String>, kind: SecretKind, value: impl Into<String>) -> Self {
        SetPasswordParams {
            account: account.into(),
            kind,
            value: value.into(),
        }
    }

    /// The parameters off the wire, or `-32602` naming what is wrong and
    /// echoing nothing: an error message is the single most likely place for a
    /// secret to escape, because it is rendered, logged and often pasted into
    /// a bug report.
    pub fn from_params(params: &Value) -> Result<Self, DomainError> {
        let account = string_param(params, "account")?;
        let kind = string_param(params, "kind")?;
        let kind = SecretKind::from_wire(&kind)
            .ok_or_else(|| DomainError::invalid_params("kind is one of \"smtp\", \"imap\""))?;
        let value = params
            .get("value")
            .and_then(Value::as_str)
            .ok_or_else(|| DomainError::invalid_params("value is a required string parameter"))?;
        Ok(SetPasswordParams::new(account, kind, value))
    }

    /// The account the password belongs to.
    pub fn account(&self) -> &str {
        &self.account
    }

    /// Which credential it is.
    pub fn kind(&self) -> SecretKind {
        self.kind
    }

    /// The value, readable by the one caller that stores it.
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl std::fmt::Debug for SetPasswordParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetPasswordParams")
            .field("account", &self.account)
            .field("kind", &self.kind)
            .field("value", &REDACTED)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// The family
// ---------------------------------------------------------------------------

/// Everything the six methods share: the configuration they own, the runtimes
/// they reconcile, and the fan-out they publish through.
pub struct ConfigFamily {
    /// The live configuration.
    pub store: Arc<ConfigStore>,
    /// The runtimes a swap stops and starts.
    pub runtimes: Arc<RuntimeTable>,
    /// Where `config.changed` and `config.invalid` go.
    pub canonical: Arc<CanonicalState>,
}

/// One served method of the family, dispatched by the name its spec declares.
pub struct ConfigMethod {
    spec: MethodSpec,
    family: Arc<ConfigFamily>,
}

/// Register the six methods from the one array that declares them.
pub fn register(dispatcher: &mut Dispatcher, family: Arc<ConfigFamily>) {
    for spec in CONFIG_METHOD_SPECS {
        dispatcher.register(Arc::new(ConfigMethod {
            spec,
            family: Arc::clone(&family),
        }));
    }
}

impl Method for ConfigMethod {
    fn spec(&self) -> MethodSpec {
        self.spec
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        let family = Arc::clone(&self.family);
        Box::pin(async move {
            match self.spec.name {
                "config.get" => Ok(Outcome::query(family.store.get_json())),
                "config.validate" => Ok(Outcome::query(validate(&family, &params)?)),
                "config.reload" => {
                    let (plan, _) = swap(&family, Source::Disk).await?;
                    Ok(command(&family, plan.to_json(), plan.affected()))
                }
                "config.set_password" => set_password(&family, &params).await,
                "config.add_account" => add_account(&family, &params).await,
                "config.init" => init(&family, &params).await,
                other => Err(DomainError::method_not_found(other)),
            }
        })
    }
}

/// The outcome of a write: the state revision it left the daemon at, and the
/// accounts whose cached copies are now stale.
fn command(family: &ConfigFamily, result: Value, affected: Vec<ResourceId>) -> Outcome {
    Outcome::command(result, family.canonical.revision().get(), affected)
}

// ---------------------------------------------------------------------------
// config.validate
// ---------------------------------------------------------------------------

/// Whether a candidate string would load. `errors` is never empty when `ok` is
/// false, and a TOML parser stops at the first syntax error, so the list is
/// what the checks produced rather than everything that is wrong.
fn validate(family: &ConfigFamily, params: &Value) -> Result<Value, DomainError> {
    let text = string_param(params, "toml")?;
    match validate_document(&text, family.store.path()) {
        Ok(_) => Ok(json!({"ok": true})),
        Err(diagnostic) => Ok(json!({
            "ok": false,
            "errors": [{"line": diagnostic.line, "message": diagnostic.message}],
        })),
    }
}

// ---------------------------------------------------------------------------
// config.set_password
// ---------------------------------------------------------------------------

/// Store one password through the backend the configuration selects.
///
/// No event: a stored password changes nothing a client can observe, because
/// `config.get` said `<redacted>` before and says `<redacted>` after. It is a
/// `Command` rather than a `Query` because it writes, and its `affected` names
/// the account so a client holding a per-account view re-reads whatever depends
/// on credentials.
async fn set_password(family: &ConfigFamily, params: &Value) -> Result<Outcome, DomainError> {
    let params = SetPasswordParams::from_params(params)?;
    let snapshot = family.store.snapshot();
    if !snapshot
        .accounts
        .iter()
        .any(|account| account.name == params.account())
    {
        return Err(DomainError::new(
            ErrorCode::AccountUnknown,
            format!("no account named {} is configured", params.account()),
            Some(json!({"account": params.account()})),
        ));
    }

    let key = params.kind().secret_key(params.account());
    let backend = snapshot.config.secrets_backend;
    let stored = {
        let key = key.clone();
        let value = params.value().to_string();
        tokio::task::spawn_blocking(move || {
            // The daemon opens the backend on first use rather than at startup:
            // a first run has no configuration to select one from, and the
            // opener is idempotent.
            crate::secrets::init(backend).map_err(|e| e.to_string())?;
            crate::config::set_secret(&key, &value).map_err(|e| format!("{e:#}"))
        })
        .await
    };
    // Neither arm renders the value: the failure is about the backend, and the
    // one thing that must not travel with it is what was being stored.
    match stored {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            return Err(DomainError::internal(format!(
                "storing the {} password of {}: {e}",
                params.kind().as_str(),
                params.account()
            )))
        }
        Err(e) => return Err(DomainError::internal(format!("the secrets task {e}"))),
    }
    info!(
        "[daemon] stored the {} password of {}",
        params.kind().as_str(),
        params.account()
    );

    Ok(command(
        family,
        json!({
            "stored": true,
            "account": params.account(),
            "kind": params.kind().as_str(),
            "key": key,
        }),
        vec![ResourceId::new(format!("account:{}", params.account()))],
    ))
}

// ---------------------------------------------------------------------------
// config.add_account and config.init
// ---------------------------------------------------------------------------

/// Append one `[[accounts]]` block to an existing `config.toml`.
async fn add_account(family: &ConfigFamily, params: &Value) -> Result<Outcome, DomainError> {
    let path = family.store.path().to_path_buf();
    if !path.exists() {
        return Err(DomainError::invalid_params(format!(
            "there is no configuration at {}; write one with config.init first",
            path.display()
        )));
    }
    let account = params
        .get("account")
        .ok_or_else(|| DomainError::invalid_params("account is a required object parameter"))?;
    let name = string_param(account, "name")?;
    if family
        .store
        .snapshot()
        .accounts
        .iter()
        .any(|configured| configured.name == name)
    {
        return Err(DomainError::invalid_params(format!(
            "an account named {name} is already configured"
        )));
    }
    let existing = fs::read_to_string(&path)
        .map_err(|e| DomainError::internal(format!("reading {}: {e}", path.display())))?;
    let document = format!("{existing}\n{}", account_block(account)?);
    let (plan, _) = swap(family, Source::Document(document)).await?;
    Ok(command(family, plan.to_json(), plan.affected()))
}

/// Write a whole `config.toml` for a daemon that has none.
///
/// An existing file is `-32602` naming it: `mp config init` asks "Overwrite?
/// [y/N]" and a daemon has nobody to ask, and the daemon error table has no
/// "already exists" code to invent one from.
async fn init(family: &ConfigFamily, params: &Value) -> Result<Outcome, DomainError> {
    let path = family.store.path().to_path_buf();
    if path.exists() {
        return Err(DomainError::invalid_params(format!(
            "a configuration already exists at {}; edit it and call config.reload",
            path.display()
        )));
    }
    let account = params
        .get("account")
        .ok_or_else(|| DomainError::invalid_params("account is a required object parameter"))?;

    let mut document = String::new();
    if let Some(theme) = params.get("theme").and_then(Value::as_str) {
        document.push_str(&format!("theme = {}\n", quote(theme)));
    }
    if let Some(notifications) = params.get("notifications").and_then(Value::as_bool) {
        document.push_str(&format!("notifications = {notifications}\n"));
    }
    if let Some(backend) = params.get("secrets_backend").and_then(Value::as_str) {
        document.push_str(&format!("secrets_backend = {}\n", quote(backend)));
    }
    document.push_str(&account_block(account)?);

    let (plan, _) = swap(family, Source::Document(document)).await?;
    let mut result = plan.to_json();
    result["path"] = json!(path.display().to_string());
    Ok(command(family, result, plan.affected()))
}

/// One account's `[[accounts]]` block, as a user would have written it.
///
/// Every string goes through [`quote`], so a name with a quote or a backslash
/// in it produces a document that parses rather than one that does not.
fn account_block(account: &Value) -> Result<String, DomainError> {
    let name = string_param(account, "name")?;
    let mut out = format!("\n[[accounts]]\nname = {}\n", quote(&name));
    for (key, field) in [
        ("default_from", "default_from"),
        ("auth_method", "auth_method"),
    ] {
        if let Some(value) = account.get(field).and_then(Value::as_str) {
            out.push_str(&format!("{key} = {}\n", quote(value)));
        }
    }
    if let Some(save_to_sent) = account.get("save_to_sent").and_then(Value::as_str) {
        out.push_str(&format!("save_to_sent = {}\n", quote(save_to_sent)));
    }
    if let Some(oauth2) = account.get("oauth2").filter(|value| value.is_object()) {
        out.push_str("\n[accounts.oauth2]\n");
        out.push_str(&strings(oauth2, &["client_id", "tenant_id"]));
    }
    for table in ["smtp", "imap"] {
        let Some(settings) = account.get(table).filter(|value| value.is_object()) else {
            continue;
        };
        out.push_str(&format!("\n[accounts.{table}]\n"));
        out.push_str(&strings(settings, &["host", "username"]));
        for field in ["port", "fetch_concurrency", "body_fetch_deadline_secs"] {
            if let Some(number) = settings.get(field).and_then(Value::as_u64) {
                out.push_str(&format!("{field} = {number}\n"));
            }
        }
        if let Some(flag) = settings
            .get("accept_invalid_certs")
            .and_then(Value::as_bool)
        {
            out.push_str(&format!("accept_invalid_certs = {flag}\n"));
        }
    }
    if let Some(mailboxes) = account.get("mailboxes").filter(|value| value.is_object()) {
        for role in ["inbox", "archive", "sent"] {
            if let Some(server) = mailbox_server(mailboxes.get(role)) {
                out.push_str(&format!(
                    "\n[accounts.mailboxes.{role}]\nserver = {}\n",
                    quote(&server)
                ));
            }
        }
        for extra in mailboxes
            .get("extra")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            if let Some(server) = mailbox_server(Some(extra)) {
                out.push_str(&format!(
                    "\n[[accounts.mailboxes.extra]]\nserver = {}\n",
                    quote(&server)
                ));
            }
        }
    }
    Ok(out)
}

/// A mailbox mapping given either as the server name or as `{server}`.
fn mailbox_server(value: Option<&Value>) -> Option<String> {
    let value = value?;
    let name = match value {
        Value::String(name) => name.clone(),
        _ => value.get("server")?.as_str()?.to_string(),
    };
    Some(name)
}

/// The `key = "value"` lines of every named field that is a string.
fn strings(object: &Value, fields: &[&str]) -> String {
    fields
        .iter()
        .filter_map(|field| {
            object
                .get(field)
                .and_then(Value::as_str)
                .map(|value| format!("{field} = {}\n", quote(value)))
        })
        .collect()
}

/// One TOML string literal, escaped by the TOML serialiser rather than by
/// hand.
fn quote(value: &str) -> String {
    toml::Value::String(value.to_string()).to_string()
}

// ---------------------------------------------------------------------------
// The swap
// ---------------------------------------------------------------------------

/// Where the candidate configuration comes from.
enum Source {
    /// `config.toml` as it is on disk, which is `config.reload`.
    Disk,
    /// A document to validate and, if it loads, write to `config.toml`.
    Document(String),
}

/// Validate a candidate, install it, reconcile the runtimes, and announce it.
///
/// The announcement is last and there is always exactly one, even when all
/// three lists are empty: a client that asked for a reload learns it happened,
/// and a no-op reload is a fact rather than silence. It is a lifecycle event,
/// so two swaps never coalesce into one and a slow client is never shown the
/// second swap's lists in place of the first's.
async fn swap(family: &ConfigFamily, source: Source) -> Result<(Reconcile, u64), DomainError> {
    let _guard = family.store.lock_swap().await;
    let path = family.store.path().to_path_buf();

    let (state, config) = match &source {
        Source::Disk if !path.exists() => (ConfigState::Absent, GlobalConfig::default()),
        Source::Disk => {
            let text = fs::read_to_string(&path)
                .map_err(|e| DomainError::internal(format!("reading {}: {e}", path.display())))?;
            (ConfigState::Ok, load(family, &path, &text)?)
        }
        Source::Document(text) => (ConfigState::Ok, load(family, &path, text)?),
    };

    // Written only now: a candidate that does not load never reaches the file,
    // so a refusal leaves it byte-identical.
    if let Source::Document(text) = &source {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                DomainError::internal(format!("creating {}: {e}", parent.display()))
            })?;
        }
        fs::write(&path, text)
            .map_err(|e| DomainError::internal(format!("writing {}: {e}", path.display())))?;
    }

    let plan = reconcile(
        &family.store,
        &family.runtimes,
        &family.canonical,
        state,
        config,
    )
    .await;
    let revision = family.store.snapshot().revision;
    info!(
        "[daemon] configuration revision {revision}: +{:?} ~{:?} -{:?}",
        plan.added, plan.updated, plan.removed
    );
    family.canonical.publish(Event::Lifecycle {
        kind: KIND_CONFIG_CHANGED,
        payload: serde_json::to_value(ConfigChanged {
            added: plan.added.clone(),
            updated: plan.updated.clone(),
            removed: plan.removed.clone(),
            config_revision: revision,
        })
        .unwrap_or_else(|_| json!({})),
    });
    Ok((plan, revision))
}

/// Parse one candidate, publishing the diagnostic a refusal produces before it
/// travels back as `-32007`: one shape, so a watcher and a caller render one
/// diagnostic.
fn load(family: &ConfigFamily, path: &Path, text: &str) -> Result<GlobalConfig, DomainError> {
    validate_document(text, path).map_err(|diagnostic| refuse(family, path, diagnostic))
}

/// Publish `config.invalid` and build the `-32007` that carries the same three
/// fields. The previous snapshot stays live and its revision does not move,
/// because nothing was swapped.
fn refuse(family: &ConfigFamily, path: &Path, diagnostic: Diagnostic) -> DomainError {
    let payload = ConfigInvalid {
        path: path.display().to_string(),
        line: diagnostic.line,
        message: diagnostic.message,
    };
    family.canonical.publish(Event::Lifecycle {
        kind: KIND_CONFIG_INVALID,
        payload: serde_json::to_value(&payload).unwrap_or_else(|_| json!({})),
    });
    DomainError::new(
        ErrorCode::ConfigInvalid,
        payload.message.clone(),
        Some(json!({
            "path": payload.path,
            "line": payload.line,
            "message": payload.message,
        })),
    )
}

/// A required string parameter, or `-32602` naming it.
fn string_param(params: &Value, name: &str) -> Result<String, DomainError> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            DomainError::invalid_params(format!("{name} is a required string parameter"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The six specs are the six names, in the order the dispatcher keeps.
    #[test]
    fn the_specs_are_declared_in_name_order() {
        let names: Vec<&str> = CONFIG_METHOD_SPECS.iter().map(|spec| spec.name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        assert_eq!(names.len(), 6);
    }

    /// The rendered block parses back into the account it described, quoting
    /// included.
    #[test]
    fn an_account_block_round_trips_through_the_loader() {
        let block = account_block(&json!({
            "name": "al\"pha",
            "default_from": "alpha@example.com",
            "smtp": {"host": "smtp.example.com", "port": 587},
            "imap": {"body_fetch_deadline_secs": 12},
            "mailboxes": {"inbox": "INBOX", "extra": ["News"]},
        }))
        .expect("a well-formed account renders");
        let config: GlobalConfig = toml::from_str(&block).expect("and parses back");
        assert_eq!(config.accounts[0].name, "al\"pha");
        assert_eq!(config.accounts[0].smtp.port, 587);
        assert_eq!(config.accounts[0].imap.body_fetch_deadline_secs, 12);
        assert_eq!(
            config.accounts[0]
                .mailboxes
                .extra
                .as_ref()
                .expect("one extra mailbox")[0]
                .server,
            "News"
        );
    }

    /// A block with no name is `-32602` rather than a document that parses
    /// into an account nothing can address.
    #[test]
    fn an_account_without_a_name_is_refused() {
        let error = account_block(&json!({"default_from": "a@b.c"})).expect_err("no name");
        assert_eq!(error.code(), -32602);
    }
}
