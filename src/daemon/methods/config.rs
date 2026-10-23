//! The `config.*` family: read, validate, swap, and the one path into the
//! secrets backend (P3b-U8).
//!
//! Ten methods, declared once in [`CONFIG_METHOD_SPECS`] and registered from
//! it, so a method cannot be served under a name the array does not carry;
//! the six of P3b-U8 are described here, the admin slice's three and
//! `config.probe` ([`super::config_probe`]) where they are served.
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
//!   backend is one path to audit. `config.init` writes over an existing
//!   file only when the caller sends `replace: true`.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use futures::future::BoxFuture;
use log::{info, warn};
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

/// The ten methods this family serves, in the name order the dispatcher's
/// table keeps.
///
/// `config.probe` came with the daemon leftovers (2026-10-22): the setup
/// wizards dial the account the user is describing before it is written, and
/// a session on a mail server is the daemon's to open
/// ([`super::config_probe`]).
///
/// The admin slice (P4-U14) added the last three: `config.cutover`, because
/// `docs/parity-matrix.md` MIG-01 offers exactly two homes for `mp cutover` and
/// the client-side one is gone once the daemon owns the data directory and the
/// drafts index the import writes into; `config.oauth2_login`, because a
/// device-code flow is authentication against the configured account; and
/// `config.reset_secrets`, because unlinking the secrets file and the token
/// caches is one committed change with nothing to watch.
pub const CONFIG_METHOD_SPECS: [MethodSpec; 10] = [
    MethodSpec::new("config.add_account", MethodKind::Command, 1),
    MethodSpec::new("config.cutover", MethodKind::Operation, 1),
    MethodSpec::new("config.get", MethodKind::Query, 1),
    MethodSpec::new("config.init", MethodKind::Command, 1),
    MethodSpec::new("config.oauth2_login", MethodKind::Operation, 1),
    MethodSpec::new("config.probe", MethodKind::Query, 1),
    MethodSpec::new("config.reload", MethodKind::Command, 1),
    MethodSpec::new("config.reset_secrets", MethodKind::Command, 1),
    MethodSpec::new("config.set_password", MethodKind::Command, 1),
    MethodSpec::new("config.validate", MethodKind::Query, 1),
];

/// The phase `config.oauth2_login` reports the verification URL and the user
/// code under.
///
/// The payload is the progress shape every operation shares,
/// `{operation_id, phase, done, total, message}`, with the two values in the
/// one free-text field separated by the single space neither of them can
/// contain. Inventing a field for this one operation would move the frame shape
/// every other operation shares; a two-token message does not.
pub const DEVICE_CODE_PHASE: &str = "device_code";

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
    /// The draft watcher, whose roots a swap re-derives.
    pub watch: Arc<crate::daemon::watch::DraftWatch>,
    /// The registry the family's two operations are started in.
    pub operations: Arc<crate::daemon::operations::OperationRegistry>,
    /// The health checks a swap re-evaluates (P6-U8).
    pub diagnostics: Arc<crate::daemon::diagnostics::Diagnostics>,
    /// The server copies a swap drops for a removed or changed account.
    pub copies: Arc<crate::daemon::server_copies::ServerCopies>,
}

/// One served method of the family, dispatched by the name its spec declares.
pub struct ConfigMethod {
    spec: MethodSpec,
    family: Arc<ConfigFamily>,
}

/// Register the ten methods from the one array that declares them.
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
        ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        let family = Arc::clone(&self.family);
        Box::pin(async move {
            match self.spec.name {
                "config.get" => Ok(Outcome::query(family.store.get_json())),
                "config.validate" => Ok(Outcome::query(validate(&family, &params)?)),
                "config.reload" => {
                    let swapped = swap(&family, Source::Disk).await;
                    // Before the refusal is returned and whether or not there
                    // was one: a reload of a file that no longer loads is
                    // exactly the case `config_loaded` exists to report, and
                    // the daemon keeps serving the configuration it has.
                    family.diagnostics.refresh();
                    let (plan, _) = swapped?;
                    Ok(command(&family, plan.to_json(), plan.affected()))
                }
                "config.set_password" => set_password(&family, &params).await,
                "config.add_account" => add_account(&family, &params).await,
                "config.init" => init(&family, &params).await,
                "config.probe" => {
                    let snapshot = family.store.snapshot();
                    let params =
                        super::config_probe::ProbeParams::from_params(&params, &snapshot.accounts)?;
                    Ok(Outcome::query(super::config_probe::probe(params).await))
                }
                "config.reset_secrets" => reset_secrets(&family, &params),
                "config.cutover" => {
                    let work = plan_cutover(&family, &params)?;
                    Ok(self.operation(ctx, move |handle| cutover(work, handle)))
                }
                "config.oauth2_login" => {
                    let work = plan_oauth2(&family.store.snapshot().accounts, &params)?;
                    Ok(self.operation(ctx, move |handle| oauth2_login(work, handle)))
                }
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
            // Startup opened the backend if a configuration had loaded; a
            // first run had none to select one from, so it opens here. The
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
    let promote = staged_sign_in(account, &name);
    let (plan, _) = swap(family, Source::Document { document, promote }).await?;
    Ok(command(family, plan.to_json(), plan.affected()))
}

/// Write a whole `config.toml` for a daemon that has none.
///
/// An existing file is `-32602` naming it unless `replace` is `true`, which is
/// `mp config init`'s "Overwrite? [y/N]" answered yes in the client, the one
/// process that has somebody to ask; the daemon error table has no "already
/// exists" code to invent one from.
async fn init(family: &ConfigFamily, params: &Value) -> Result<Outcome, DomainError> {
    guard(
        "config.init",
        params,
        &[
            "account",
            "notifications",
            "replace",
            "secrets_backend",
            "theme",
        ],
    )?;
    let path = family.store.path().to_path_buf();
    let replace = params
        .get("replace")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if path.exists() && !replace {
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

    let promote = staged_sign_in(account, &string_param(account, "name")?);
    let (plan, _) = swap(family, Source::Document { document, promote }).await?;
    let mut result = plan.to_json();
    result["path"] = json!(path.display().to_string());
    Ok(command(family, result, plan.affected()))
}

/// The account whose staged sign-in a write promotes: the block's own name,
/// when the block is a Graph account, the one kind that signs in with a token.
///
/// The setup wizard signs a candidate in before it writes the account
/// (`config.oauth2_login` with a `candidate`), and that sign-in is staged
/// ([`crate::oauth2::staged_token_name`]) so an abandoned wizard leaves the
/// token of a configured account of that name untouched. The write that
/// commits the account is the one moment the staged token becomes the
/// account's.
fn staged_sign_in(account: &Value, name: &str) -> Option<String> {
    (account.get("auth_method").and_then(Value::as_str) == Some("graph")).then(|| name.to_string())
}

/// The account a candidate `[[accounts]]` block describes, read through the
/// loader a written file goes through, so a probe or a sign-in of a
/// candidate and the write that follows it agree about every default.
pub fn candidate_account(candidate: &Value) -> Result<crate::config::AccountConfig, DomainError> {
    let block = account_block(candidate)?;
    let loaded: GlobalConfig = toml::from_str(&block).map_err(|e| {
        DomainError::invalid_params(format!("the candidate account does not load: {e}"))
    })?;
    loaded
        .accounts
        .into_iter()
        .next()
        .ok_or_else(|| DomainError::invalid_params("account is a required object parameter"))
}

/// One account's `[[accounts]]` block, as a user would have written it.
///
/// Every string goes through [`quote`], so a name with a quote or a backslash
/// in it produces a document that parses rather than one that does not.
///
/// A block naming the retired `auth_method = "oauth2"` is refused with the
/// sentence that says how to switch, so no wizard, probe or sign-in takes one
/// and no write adds one to the file.
fn account_block(account: &Value) -> Result<String, DomainError> {
    let name = string_param(account, "name")?;
    if account.get("auth_method").and_then(Value::as_str) == Some("oauth2") {
        return Err(DomainError::invalid_params(
            crate::config::retired_oauth2_message(&name),
        ));
    }
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
        for field in [
            "port",
            "fetch_concurrency",
            "body_fetch_deadline_secs",
            "sync_interval_secs",
        ] {
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
        // The three synced roles, then the roles a server usually marks
        // itself (#0141): a `drafts`, `trash` or `junk` key names the folder
        // only when the server does not.
        for role in ["inbox", "archive", "sent", "drafts", "trash", "junk"] {
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
        for rule in mailboxes
            .get("overrides")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let pattern = string_param(rule, "match")?;
            let policy = string_param(rule, "policy")?;
            if crate::config::FolderPolicy::parse(&policy).is_none() {
                return Err(DomainError::invalid_params(format!(
                    "the override for '{pattern}' has policy '{policy}'; \
                     a policy is sync, status-only or ignore"
                )));
            }
            out.push_str(&format!(
                "\n[[accounts.mailboxes.override]]\nmatch = {}\npolicy = {}\n",
                quote(&pattern),
                quote(&policy)
            ));
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
    /// A document to validate and, if it loads, write to `config.toml`, and
    /// the account whose staged sign-in becomes its token once it is written.
    Document {
        document: String,
        promote: Option<String>,
    },
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
        Source::Document { document, .. } => (ConfigState::Ok, load(family, &path, document)?),
    };

    // Written only now: a candidate that does not load never reaches the file,
    // so a refusal leaves it byte-identical.
    if let Source::Document {
        document: text,
        promote,
    } = &source
    {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                DomainError::internal(format!("creating {}: {e}", parent.display()))
            })?;
        }
        // Atomic and mode-preserving: a crash mid-write leaves the previous
        // file whole, and a config the user chmod'ed stays that way.
        crate::draft::write_atomic(&path, text.as_bytes())
            .map_err(|e| DomainError::internal(format!("writing {}: {e:#}", path.display())))?;
        // After the file and before the runtimes: the account's first sync
        // finds the token its wizard signed in with, and a write that was
        // refused above promoted nothing.
        if let Some(account) = promote {
            crate::oauth2::promote_staged_token(account)
                .map_err(|e| DomainError::internal(format!("{e:#}")))?;
        }
    }

    let plan = reconcile(
        &family.store,
        &family.runtimes,
        &family.canonical,
        &family.watch,
        &family.copies,
        state,
        config,
    )
    .await;
    // An added account, or a changed one whose earlier first sync failed (the
    // credentials were wrong, say), gets its first sync now.
    for account in plan.added.iter().chain(&plan.updated) {
        crate::daemon::config::spawn_first_sync_if_new(
            &family.store,
            &family.runtimes,
            &family.canonical,
            account,
        );
    }
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
    let config =
        validate_document(text, path).map_err(|diagnostic| refuse(family, path, diagnostic))?;
    // What the load ignored but still took (PERSO-118): a line in the log per
    // finding, as `load_global_config` writes at startup.
    for warning in crate::config::config_warnings(text) {
        warn!("[daemon] {}: {warning}", path.display());
    }
    Ok(config)
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

// ---------------------------------------------------------------------------
// The admin slice: cutover, secrets reset, device-code login (P4-U14)
// ---------------------------------------------------------------------------

impl ConfigMethod {
    /// Start one operation of this family and answer with its id and nothing
    /// else, exactly as `send.*` and `sync.*` do.
    fn operation<F, Fut>(&self, ctx: &ClientCtx, work: F) -> Outcome
    where
        F: FnOnce(crate::daemon::operations::OperationHandle) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let (id, handle) = self.family.operations.start(
            crate::daemon::state::ConnectionId(ctx.connection_id),
            self.spec.cancel_scope,
            self.spec.name,
        );
        tokio::spawn(work(handle));
        Outcome::query(json!({"operation_id": id.as_str()}))
    }
}

/// One validated cutover pass.
struct CutoverWork {
    account: String,
    dry_run: bool,
}

/// Validate `config.cutover`: a configured account with a store on disk, and
/// nothing else. The all-accounts loop is the client's, in configuration order.
fn plan_cutover(family: &ConfigFamily, params: &Value) -> Result<CutoverWork, DomainError> {
    guard("config.cutover", params, &["account", "dry_run"])?;
    let name = string_param(params, "account")?;
    let snapshot = family.store.snapshot();
    super::account::ready_account(&snapshot.accounts, &name).map_err(DomainError::from)?;
    Ok(CutoverWork {
        account: name,
        dry_run: params
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// `MIG-01`: give every file-era draft an `id:` and name what is left of the
/// file-era mailstore. It deletes nothing, ever, and `dry_run` writes not even
/// the `id:` field.
async fn cutover(work: CutoverWork, handle: crate::daemon::operations::OperationHandle) {
    handle.set_running();
    let name = work.account.clone();
    match tokio::task::spawn_blocking(move || cutover_blocking(&work)).await {
        Ok(Ok(result)) => handle.succeed(result),
        Ok(Err(e)) => handle.fail(DomainError::internal(format!("{e:#}"))),
        Err(e) => handle.fail(DomainError::internal(format!("the cutover of {name} {e}"))),
    }
}

/// The blocking half of a cutover.
fn cutover_blocking(work: &CutoverWork) -> anyhow::Result<Value> {
    let account = &work.account;
    let store = crate::store::Store::open(crate::config::store_path(account))?;
    let report = crate::cutover::cutover_account(
        &store,
        account,
        &crate::config::account_dir(account),
        &crate::config::drafts_dir(account),
        work.dry_run,
    )?;
    Ok(json!({
        "account": account,
        "dry_run": work.dry_run,
        "drafts": {
            "imported": report
                .drafts
                .imported
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>(),
            "already_indexed": report.drafts.already_indexed,
            // Rendered here rather than structured: both are `Display` types
            // the CLI prints verbatim, and a client that re-worded them would
            // be inventing a second spelling of one fact.
            "skipped": report
                .drafts
                .skipped
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            "collisions": report
                .drafts
                .collisions
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
        },
        "remnants": report
            .remnants
            .iter()
            .map(|remnant| json!({
                "path": remnant.path.display().to_string(),
                "md_files": remnant.md_files,
                "bytes": remnant.bytes,
            }))
            .collect::<Vec<_>>(),
    }))
}

/// `ACC-07`: unlink the encrypted secrets file and every OAuth2 token cache,
/// and name what went.
///
/// The secrets file first, then the token caches in path order, which is the
/// order `mp config reset-secrets` prints them; the pre-daemon binary walked
/// `read_dir` unsorted, which was only ever deterministic because a real
/// installation has one cache per account. Idempotent: a second reset removes
/// nothing and says so.
fn reset_secrets(family: &ConfigFamily, params: &Value) -> Result<Outcome, DomainError> {
    guard("config.reset_secrets", params, &[])?;
    let mut removed: Vec<String> = Vec::new();

    let secrets_file = crate::secrets::secrets_path();
    if secrets_file.exists() {
        fs::remove_file(&secrets_file).map_err(|e| {
            DomainError::internal(format!("removing {}: {e}", secrets_file.display()))
        })?;
        removed.push(secrets_file.display().to_string());
    }

    // The configured accounts' caches, then the sign-ins a wizard staged and
    // never committed, which are tokens as much as the others.
    let token_dir = crate::config::tokens_dir();
    for dir in [
        token_dir.clone(),
        token_dir.join(crate::oauth2::STAGED_TOKEN_DIR),
    ] {
        if !dir.exists() {
            continue;
        }
        let mut caches: Vec<std::path::PathBuf> = fs::read_dir(&dir)
            .map_err(|e| DomainError::internal(format!("reading {}: {e}", dir.display())))?
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("enc"))
            .collect();
        caches.sort();
        for path in caches {
            fs::remove_file(&path)
                .map_err(|e| DomainError::internal(format!("removing {}: {e}", path.display())))?;
            removed.push(path.display().to_string());
        }
    }

    // The backend is reopened so the next `config.set_password` writes a fresh
    // file under the current machine key rather than reusing the handle on the
    // one just unlinked.
    let snapshot = family.store.snapshot();
    let _ = crate::config::init_secrets_backend(&snapshot.config);
    info!("[daemon] reset secrets: removed {} file(s)", removed.len());

    Ok(command(
        family,
        json!({"removed": removed}),
        vec![ResourceId::new("config:secrets")],
    ))
}

/// One validated device-code login.
struct OAuth2Work {
    account: String,
    /// The token-cache name the sign-in is written under: the account's own,
    /// or the staged one of a candidate.
    token: String,
    /// Whether this is a candidate's sign-in, staged until the write.
    staged: bool,
    client_id: String,
    tenant_id: String,
}

/// Validate `config.oauth2_login`, in the three refusals `src/config_cmd/oauth2.rs`
/// made and in its own sentences: an unknown account, an account that is not
/// a Graph account, and a Graph account with no usable client. An account
/// naming the retired `auth_method = "oauth2"` is refused with the sentence
/// that says how to switch it.
///
/// The unknown-account message is the command's rather than
/// [`super::account::configured_account`]'s, because it is the sentence the user
/// reads and a routed command may not reword it.
///
/// `candidate` signs in an account that is not configured yet: the setup
/// wizard's `[[accounts]]` block, read through the loader, used instead of the
/// configured account of that name, and named `account` like it. Its token is
/// staged ([`crate::oauth2::staged_token_name`]) rather than cached under that
/// name, and `config.init` or `config.add_account` promotes it when it writes
/// the account: a wizard abandoned after its sign-in, or one whose candidate
/// is named like a configured account, leaves that account's token untouched.
fn plan_oauth2(
    accounts: &[crate::config::AccountConfig],
    params: &Value,
) -> Result<OAuth2Work, DomainError> {
    guard("config.oauth2_login", params, &["account", "candidate"])?;
    let name = string_param(params, "account")?;
    let candidate = match params.get("candidate") {
        None | Some(Value::Null) => None,
        Some(block) => {
            let account = candidate_account(block)?;
            if account.name != name {
                return Err(DomainError::invalid_params(format!(
                    "the candidate is named {}, not {name}",
                    account.name
                )));
            }
            Some(account)
        }
    };
    let account = match &candidate {
        Some(account) => account,
        None => accounts
            .iter()
            .find(|account| account.name == name)
            .ok_or_else(|| {
                DomainError::new(
                    ErrorCode::AccountUnknown,
                    format!("Account '{name}' not found in config"),
                    Some(json!({"account": name})),
                )
            })?,
    };

    match account.auth_method {
        crate::config::AuthMethod::Graph => {}
        crate::config::AuthMethod::Password => {
            return Err(DomainError::invalid_params(format!(
                "Account '{name}' uses auth_method = \"password\", not \"graph\". \
                 Set auth_method = \"graph\" in config.toml to sign in to Microsoft 365."
            )))
        }
        crate::config::AuthMethod::RetiredOAuth2 => {
            return Err(DomainError::invalid_params(
                crate::config::retired_oauth2_message(&name),
            ))
        }
    }
    let settings = account.oauth2.as_ref().ok_or_else(|| {
        DomainError::invalid_params(format!(
            "Account '{name}' requires an [accounts.oauth2] section with client_id and tenant_id."
        ))
    })?;
    if settings.client_id.is_empty() || settings.tenant_id.is_empty() {
        return Err(DomainError::invalid_params(format!(
            "OAuth2 client_id and tenant_id must be set for account '{name}'"
        )));
    }

    let staged = candidate.is_some();
    Ok(OAuth2Work {
        token: if staged {
            crate::oauth2::staged_token_name(&name)
        } else {
            name.clone()
        },
        staged,
        account: name,
        client_id: settings.client_id.clone(),
        tenant_id: settings.tenant_id.clone(),
    })
}

/// `ACC-06`: the device-code flow, with the verification URL and the user code
/// reported as the operation's first progress so the client can render them and
/// open a browser (`INT-04`).
async fn oauth2_login(work: OAuth2Work, handle: crate::daemon::operations::OperationHandle) {
    handle.set_running();
    // A candidate sign-in is a wizard starting: the sign-ins wizards staged
    // and abandoned more than a day ago go, so `candidates/` does not keep
    // them until `config.reset_secrets`.
    if work.staged {
        if let Err(e) = crate::oauth2::sweep_staged_tokens() {
            log::warn!("[daemon] could not drop the abandoned staged sign-ins: {e:#}");
        }
    }
    let report = |uri: &str, code: &str| {
        handle.report(crate::daemon::operations::Progress {
            phase: DEVICE_CODE_PHASE.to_string(),
            done: 0,
            total: None,
            message: Some(format!("{uri} {code}")),
        });
    };
    let acquired = crate::oauth2::device_code_flow_reporting(
        &work.client_id,
        &work.tenant_id,
        &work.token,
        crate::oauth2::GRAPH_SCOPES,
        &report,
    )
    .await;
    match acquired {
        Ok(_) => handle.succeed(json!({
            "stored": true,
            "staged": work.staged,
            "account": work.account,
            "kind": "graph",
            "key": format!("oauth2-token-{}", work.account),
        })),
        // The value is never rendered: the failure is about the provider, and
        // the one thing that must not travel with it is anything it handed back.
        Err(e) => handle.fail(DomainError::internal(format!("{e:#}"))),
    }
}

/// [`super::only_params`] in this family's error type.
fn guard(method: &str, params: &Value, allowed: &[&str]) -> Result<(), DomainError> {
    super::only_params(method, params, allowed).map_err(DomainError::from)
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

    /// The ten specs are the ten names, in the order the dispatcher keeps.
    #[test]
    fn the_specs_are_declared_in_name_order() {
        let names: Vec<&str> = CONFIG_METHOD_SPECS.iter().map(|spec| spec.name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        assert_eq!(names.len(), 10);
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
        assert_eq!(crate::config::config_warnings(&block), Vec::<String>::new());
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

    /// The roles a server usually marks and the policy overrides render as
    /// the keys the catalogue reads (#0141), and an unknown policy is refused
    /// rather than written into a file that would not load.
    #[test]
    fn an_account_block_carries_the_role_keys_and_the_overrides() {
        let block = account_block(&json!({
            "name": "work",
            "mailboxes": {
                "inbox": "INBOX",
                "trash": "Papierkorb",
                "junk": {"server": "Spam"},
                "overrides": [
                    {"match": "Labels/*", "policy": "ignore"},
                    {"match": "Archive/2019", "policy": "status-only"},
                ],
            },
        }))
        .expect("a well-formed account renders");
        let config: GlobalConfig = toml::from_str(&block).expect("and parses back");
        assert_eq!(crate::config::config_warnings(&block), Vec::<String>::new());
        let mailboxes = &config.accounts[0].mailboxes;
        assert_eq!(mailboxes.trash.as_ref().unwrap().server, "Papierkorb");
        assert_eq!(mailboxes.junk.as_ref().unwrap().server, "Spam");
        assert!(mailboxes.drafts.is_none());
        let rules: Vec<(&str, crate::config::FolderPolicy)> = mailboxes
            .overrides
            .iter()
            .map(|rule| (rule.pattern.as_str(), rule.policy))
            .collect();
        assert_eq!(
            rules,
            vec![
                ("Labels/*", crate::config::FolderPolicy::Ignore),
                ("Archive/2019", crate::config::FolderPolicy::StatusOnly),
            ]
        );

        let refused = account_block(&json!({
            "name": "work",
            "mailboxes": {"overrides": [{"match": "x", "policy": "sometimes"}]},
        }))
        .expect_err("an unknown policy");
        assert!(format!("{refused:?}").contains("sometimes"), "{refused:?}");
    }

    /// A candidate's sign-in is staged, a configured account's is not: a
    /// wizard whose candidate is named like a configured account, then
    /// abandoned, leaves that account's token where it was. Before the fix the
    /// candidate's token went to the configured account's own cache.
    #[test]
    fn a_candidate_sign_in_is_staged_and_a_configured_one_is_not() {
        let block = json!({
            "name": "work", "default_from": "me@example.com", "auth_method": "graph",
            "oauth2": {"client_id": "c", "tenant_id": "common"},
        });
        let configured = candidate_account(&block).expect("the account loads");
        let accounts = vec![configured];

        let candidate = plan_oauth2(&accounts, &json!({"account": "work", "candidate": block}))
            .expect("a candidate named like a configured account signs in");
        assert_eq!(candidate.token, "candidates/work");
        assert!(candidate.staged);
        assert_ne!(
            crate::oauth2::token_cache_path(&candidate.token),
            crate::oauth2::token_cache_path("work"),
            "the staged cache is not the configured account's"
        );

        let login = plan_oauth2(&accounts, &json!({"account": "work"}))
            .expect("the configured account signs in");
        assert_eq!(login.token, "work");
        assert!(!login.staged);
    }

    /// Only a Graph block promotes a staged sign-in.
    #[test]
    fn only_a_graph_account_promotes_a_staged_sign_in() {
        assert_eq!(
            staged_sign_in(&json!({"auth_method": "oauth2"}), "work"),
            None
        );
        assert_eq!(
            staged_sign_in(&json!({"auth_method": "graph"}), "work").as_deref(),
            Some("work")
        );
        assert_eq!(staged_sign_in(&json!({}), "work"), None);
        assert_eq!(
            staged_sign_in(&json!({"auth_method": "password"}), "work"),
            None
        );
    }

    /// A block naming the retired `auth_method = "oauth2"` is `-32602` with
    /// the sentence that says how to switch, whichever method takes it: the
    /// writes, a probe and a sign-in all read the block through here.
    #[test]
    fn a_block_naming_the_retired_oauth2_mode_is_refused_with_how_to_switch() {
        let block = json!({
            "name": "office", "default_from": "me@example.com", "auth_method": "oauth2",
            "oauth2": {"client_id": "c", "tenant_id": "common"},
            "smtp": {"host": "smtp.office365.com", "port": 587, "username": "me@example.com"},
        });
        let error = account_block(&block).expect_err("the retired mode");
        assert_eq!(error.code(), -32602);
        let text = format!("{error}");
        assert!(text.contains("auth_method = \"graph\""), "{text}");
        assert!(text.contains("on-premises Exchange"), "{text}");
        assert!(candidate_account(&block).is_err());
        let refused = plan_oauth2(&[], &json!({"account": "office", "candidate": block}))
            .err()
            .expect("no sign-in either");
        assert!(format!("{refused}").contains("no longer supports"));
    }

    /// A configured account naming the retired mode is not signed in either:
    /// the refusal says how to switch it.
    #[test]
    fn a_configured_retired_oauth2_account_is_not_signed_in() {
        let config: GlobalConfig = toml::from_str(
            "[[accounts]]\nname = \"office\"\nauth_method = \"oauth2\"\n\n\
             [accounts.oauth2]\nclient_id = \"c\"\ntenant_id = \"common\"\n",
        )
        .expect("a config naming the retired mode still loads");
        let refused = plan_oauth2(&config.accounts, &json!({"account": "office"}))
            .err()
            .expect("refused");
        assert!(format!("{refused}").contains("no longer supports"));
    }

    /// A block with no name is `-32602` rather than a document that parses
    /// into an account nothing can address.
    #[test]
    fn an_account_without_a_name_is_refused() {
        let error = account_block(&json!({"default_from": "a@b.c"})).expect_err("no name");
        assert_eq!(error.code(), -32602);
    }
}
