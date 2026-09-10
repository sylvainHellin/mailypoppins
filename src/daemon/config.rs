//! The configuration the daemon owns, and the swap that replaces it (P3b-U8).
//!
//! The daemon is the only component that parses a complete configuration, so
//! the configuration is daemon state rather than a file every command re-reads:
//! one [`ConfigStore`] holds the live [`Snapshot`], every read of it is a lock
//! taken and released, and a swap installs a whole new snapshot or none at all.
//!
//! Three rules the rest of the daemon depends on:
//!
//! - **A candidate is validated before anything is stopped or written.** A
//!   document that does not load leaves the previous snapshot live, moves no
//!   revision, and is reported once as `-32007` and once as a `config.invalid`
//!   event carrying the same three fields.
//! - **The config revision starts at 0 and moves by one per successful swap.**
//!   It is not the state revision: a client comparing configuration copies
//!   needs a counter that moves only when a configuration does.
//! - **Effective means after serde defaults, not after the engine's clamps.**
//!   [`effective_config`] reports what [`GlobalConfig`] holds once it has
//!   loaded, with retention resolved and both password fields replaced by
//!   [`REDACTED`](super::methods::config::REDACTED).
//!
//! [`reconcile`] is the runtime half: stop removed, update, start added, in
//! that order, each step committing its own revision, so an account can be
//! renamed in one edit without the new runtime racing the old one for the
//! engine lock.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock, RwLockReadGuard};

use log::{info, warn};
use serde_json::{json, Value};

use crate::config::{
    AccountConfig, AuthMethod, GlobalConfig, RetentionConfig, RetentionPolicy, SaveToSent,
};
use crate::secrets::SecretsBackendKind;

use super::methods::config::REDACTED;
use super::server::RuntimeTable;
use super::session::ConfigReport;
use super::state::{seeds_from_config, CanonicalState, Change};

/// Whether the daemon has a configuration, as `config.get` and the handshake
/// both spell it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigState {
    /// It loaded.
    Ok,
    /// There is no `config.toml`; the daemon serves zero accounts and the
    /// `config.*` family, which is how one gets written.
    Absent,
    /// It exists and does not load, with the one-line reason.
    Invalid(String),
}

impl ConfigState {
    /// The string this state travels as.
    pub fn as_str(&self) -> &'static str {
        match self {
            ConfigState::Ok => "ok",
            ConfigState::Absent => "absent",
            ConfigState::Invalid(_) => "invalid",
        }
    }
}

/// One whole configuration, at the revision it was installed at.
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// 0 for the configuration the daemon started with, +1 per swap.
    pub revision: u64,
    /// Which of the three cases this daemon is in.
    pub state: ConfigState,
    /// The loaded configuration, empty for `absent` and `invalid`.
    pub config: Arc<GlobalConfig>,
    /// Its accounts, shared with the read methods so they see every swap.
    pub accounts: Arc<Vec<AccountConfig>>,
}

/// The live configuration, and the only writer of it.
#[derive(Debug)]
pub struct ConfigStore {
    /// The file the daemon read, or would have read.
    path: PathBuf,
    inner: RwLock<Snapshot>,
    /// Held across a whole swap, so two reloads cannot interleave their stops
    /// and starts. Async, because a swap awaits `spawn_blocking`.
    swap: tokio::sync::Mutex<()>,
    /// Whether this daemon starts account runtimes at all (plan section 3.0).
    pub account_runtimes: bool,
}

impl ConfigStore {
    /// The store a daemon starts with.
    pub fn new(
        path: PathBuf,
        state: ConfigState,
        config: GlobalConfig,
        account_runtimes: bool,
    ) -> Self {
        let accounts = Arc::new(config.accounts.clone());
        ConfigStore {
            path,
            inner: RwLock::new(Snapshot {
                revision: 0,
                state,
                config: Arc::new(config),
                accounts,
            }),
            swap: tokio::sync::Mutex::new(()),
            account_runtimes,
        }
    }

    /// The live snapshot, cloned so no caller holds the lock.
    pub fn snapshot(&self) -> Snapshot {
        self.read().clone()
    }

    /// The configured accounts, in the file's order.
    pub fn accounts(&self) -> Arc<Vec<AccountConfig>> {
        Arc::clone(&self.read().accounts)
    }

    /// The file this daemon reads its configuration from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The accounts `daemon.status` reports, which is none at all for a daemon
    /// that starts no runtimes: there would be nothing to report a state for.
    pub fn status_accounts(&self) -> Vec<String> {
        if !self.account_runtimes {
            return Vec::new();
        }
        self.read()
            .accounts
            .iter()
            .map(|account| account.name.clone())
            .collect()
    }

    /// The `config_status` object of an `initialize` result, recomputed from
    /// the live snapshot rather than from what startup found.
    pub fn report(&self) -> ConfigReport {
        let snapshot = self.read();
        let path = self.path.clone();
        match &snapshot.state {
            ConfigState::Ok => ConfigReport::Loaded {
                path,
                accounts: snapshot.accounts.len(),
            },
            ConfigState::Absent => ConfigReport::Absent { path },
            ConfigState::Invalid(problem) => ConfigReport::Invalid {
                path,
                problem: problem.clone(),
            },
        }
    }

    /// The `result` of `config.get`.
    pub fn get_json(&self) -> Value {
        let snapshot = self.read();
        json!({
            "revision": snapshot.revision,
            "path": self.path.display().to_string(),
            "state": snapshot.state.as_str(),
            "config": effective_config(&snapshot.config),
        })
    }

    /// Install a validated configuration and return the revision it moved to.
    pub fn install(&self, state: ConfigState, config: GlobalConfig) -> u64 {
        let mut snapshot = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        snapshot.revision += 1;
        snapshot.accounts = Arc::new(config.accounts.clone());
        snapshot.config = Arc::new(config);
        snapshot.state = state;
        snapshot.revision
    }

    /// Take the swap lock, so one reload at a time reconciles runtimes.
    pub async fn lock_swap(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.swap.lock().await
    }

    fn read(&self) -> RwLockReadGuard<'_, Snapshot> {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Why a candidate was refused: the 1-based line of the offending token when
/// the diagnostic has a span, and a message a user can act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// The offending line, `None` for a semantic refusal.
    pub line: Option<u32>,
    /// One line for the user.
    pub message: String,
}

/// Whether `text` would load, as a pure function of the string.
///
/// The same three checks [`crate::config::load_global_config`] runs - legacy
/// keys, the TOML parse, retention - against the parameter instead of against
/// the file, so `config.validate` answers exactly what a reload would decide.
pub fn validate_document(text: &str, path: &Path) -> Result<GlobalConfig, Diagnostic> {
    if let Err(e) = crate::config::reject_legacy_keys(text, path) {
        return Err(Diagnostic {
            line: None,
            message: format!("{e:#}"),
        });
    }
    let config: GlobalConfig = toml::from_str(text).map_err(|e| Diagnostic {
        line: e.span().map(|span| line_of(text, span.start)),
        message: e.message().to_string(),
    })?;
    crate::config::validate_retention(&config).map_err(|e| Diagnostic {
        line: None,
        message: format!("{e:#}"),
    })?;
    Ok(config)
}

/// The 1-based line byte offset `at` falls on.
fn line_of(text: &str, at: usize) -> u32 {
    let at = at.min(text.len());
    (text[..at].matches('\n').count() + 1) as u32
}

// ---------------------------------------------------------------------------
// The effective configuration
// ---------------------------------------------------------------------------

/// The whole configuration as `config.get` reports it: serde defaults applied,
/// retention resolved, both password fields redacted.
pub fn effective_config(config: &GlobalConfig) -> Value {
    json!({
        "theme": config.theme,
        "notifications": config.notifications,
        "secrets_backend": match config.secrets_backend {
            SecretsBackendKind::EncryptedFile => "encrypted-file",
            SecretsBackendKind::Keyring => "keyring",
        },
        "email": {
            "font_family": config.email.font_family,
            "font_size": config.email.font_size,
            "include_signature": config.email.include_signature,
            "send_hold_secs": config.email.send_hold_secs,
        },
        "retention": retention_json(
            &RetentionPolicy::resolve(&config.retention, &RetentionConfig::default())
                .unwrap_or_default(),
        ),
        "accounts": config
            .accounts
            .iter()
            .map(|account| effective_account(config, account))
            .collect::<Vec<_>>(),
    })
}

/// One account as `config.get` reports it, which is also what
/// [`reconcile`] compares to decide whether an account was *updated*.
pub fn effective_account(config: &GlobalConfig, account: &AccountConfig) -> Value {
    let retention = crate::config::retention_for(config, account).unwrap_or_default();
    json!({
        "name": account.name,
        "default_from": account.default_from,
        "auth_method": match account.auth_method {
            AuthMethod::Password => "password",
            AuthMethod::OAuth2 => "oauth2",
            AuthMethod::Graph => "graph",
        },
        // A public identifier, never redacted: `mp config show` prints it in
        // the clear and it is the one thing an OAuth2 setup is diagnosed from.
        "oauth2": account.oauth2.as_ref().map(|settings| json!({
            "client_id": settings.client_id,
            "tenant_id": settings.tenant_id,
        })),
        "smtp": {
            "host": account.smtp.host,
            "port": account.smtp.port,
            "username": account.smtp.username,
            "accept_invalid_certs": account.smtp.accept_invalid_certs,
            // Present and always the literal: an absent field would say "no
            // password is stored", which is a fact about the secrets backend
            // and one a redacted read must not go and look up.
            "password": REDACTED,
        },
        "imap": {
            "host": account.imap.host,
            "port": account.imap.port,
            "username": account.imap.username,
            "accept_invalid_certs": account.imap.accept_invalid_certs,
            "fetch_concurrency": account.imap.fetch_concurrency,
            "body_fetch_deadline_secs": account.imap.body_fetch_deadline_secs,
            "password": REDACTED,
        },
        "mailboxes": {
            "inbox": mapping_json(account.mailboxes.inbox.as_ref()),
            "archive": mapping_json(account.mailboxes.archive.as_ref()),
            "sent": mapping_json(account.mailboxes.sent.as_ref()),
            "extra": account
                .mailboxes
                .extra
                .as_ref()
                .map(|list| {
                    list.iter()
                        .map(|m| json!({"server": m.server}))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        },
        "retention": retention_json(&retention),
        "save_to_sent": match account.save_to_sent {
            SaveToSent::Auto => "auto",
            SaveToSent::Always => "always",
            SaveToSent::Never => "never",
        },
    })
}

/// One mailbox mapping, `null` for a role nothing maps.
fn mapping_json(mapping: Option<&crate::config::MailboxMapping>) -> Value {
    mapping
        .map(|mapping| json!({"server": mapping.server}))
        .unwrap_or(Value::Null)
}

/// A resolved retention policy, which is what the engine acts on.
fn retention_json(policy: &RetentionPolicy) -> Value {
    json!({
        "metadata_horizon_days": policy.metadata_horizon_days,
        "body_horizon_days": policy.body_horizon_days,
        "attachment_horizon_days": policy.attachment_horizon_days,
        "max_disk_bytes": policy.max_disk_bytes,
    })
}

// ---------------------------------------------------------------------------
// Reconciliation
// ---------------------------------------------------------------------------

/// What one swap did to the runtime table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reconcile {
    /// Accounts started, sorted.
    pub added: Vec<String>,
    /// Accounts whose effective configuration changed, sorted.
    pub updated: Vec<String>,
    /// Accounts stopped, sorted.
    pub removed: Vec<String>,
}

impl Reconcile {
    /// The `result` of `config.reload` and `config.add_account`.
    pub fn to_json(&self) -> Value {
        json!({"added": self.added, "updated": self.updated, "removed": self.removed})
    }

    /// Every account the swap touched, as the resources it invalidated.
    pub fn affected(&self) -> Vec<super::dispatch::ResourceId> {
        self.added
            .iter()
            .chain(&self.updated)
            .chain(&self.removed)
            .map(|name| super::dispatch::ResourceId::new(format!("account:{name}")))
            .collect()
    }

    /// Which accounts changed, comparing the effective JSON of each: comparing
    /// the loaded structs would need a `PartialEq` the config types do not
    /// have, and comparing the raw text would call a reformatted file a change.
    fn between(previous: &GlobalConfig, next: &GlobalConfig) -> Reconcile {
        let mut reconcile = Reconcile::default();
        for account in &next.accounts {
            match previous
                .accounts
                .iter()
                .find(|other| other.name == account.name)
            {
                None => reconcile.added.push(account.name.clone()),
                Some(before) => {
                    if effective_account(previous, before) != effective_account(next, account) {
                        reconcile.updated.push(account.name.clone());
                    }
                }
            }
        }
        for account in &previous.accounts {
            if !next.accounts.iter().any(|other| other.name == account.name) {
                reconcile.removed.push(account.name.clone());
            }
        }
        for list in [
            &mut reconcile.added,
            &mut reconcile.updated,
            &mut reconcile.removed,
        ] {
            list.sort();
        }
        reconcile
    }
}

/// Install `config` and bring the runtimes in line with it.
///
/// The order is stop-removed, then update, then start-added, and every step
/// commits its own revision, so everything the swap did is in front of the
/// `config.changed` the caller publishes afterwards. It returns only once every
/// runtime it touched has settled: a stopped one is dropped and its engine lock
/// is free, a started one is `ready` or `blocked`.
pub async fn reconcile(
    store: &ConfigStore,
    runtimes: &Arc<RuntimeTable>,
    canonical: &Arc<CanonicalState>,
    watch: &Arc<super::watch::DraftWatch>,
    state: ConfigState,
    config: GlobalConfig,
) -> Reconcile {
    let previous = store.snapshot();
    let plan = Reconcile::between(&previous.config, &config);

    for account in &plan.removed {
        stop_account(runtimes, account).await;
        canonical.publish(super::state::events::Event::Remove {
            resource: super::dispatch::ResourceId::new(format!("account:{account}")),
        });
    }

    // Before any runtime reports: a change naming an account the state does
    // not have is dropped, so gamma must exist in the state before its
    // readiness is committed, and alpha must be gone before the swap is over.
    store.install(state, config);
    canonical.reseed(seeds_from_config(&store.accounts()));
    // The watched roots are one per configured account, so a swap that added
    // or removed one moves them: an added account's drafts are watched from
    // the next poll, and a removed account's are forgotten with its rows.
    watch.set_roots(super::watch::roots_from(&store.accounts()));

    let accounts = store.accounts();
    let of = |name: &String| accounts.iter().find(|a| &a.name == name).cloned();
    for name in &plan.updated {
        stop_account(runtimes, name).await;
        if let Some(cfg) = of(name) {
            start_account(runtimes, canonical, cfg, store.account_runtimes).await;
        }
    }
    for name in &plan.added {
        if let Some(cfg) = of(name) {
            start_account(runtimes, canonical, cfg, store.account_runtimes).await;
        }
    }
    plan
}

/// Drop one account's runtime, off the reactor: the drop closes SQLite and
/// releases the engine lock, and the caller's promise is that both have
/// happened by the time the reload answers.
async fn stop_account(runtimes: &Arc<RuntimeTable>, account: &str) {
    let Some(runtime) = runtimes.remove(account) else {
        return;
    };
    info!("[daemon] stopping the runtime for {account}");
    if let Err(e) = tokio::task::spawn_blocking(move || drop(runtime)).await {
        warn!("[daemon] the stop task for {account} {e}");
    }
}

/// Start one account's runtime and commit what it reported.
///
/// The table is filled *before* the change is applied, so a client that reacts
/// to the event and immediately asks `daemon.status` cannot be told the account
/// is still opening. A daemon that starts no runtimes commits nothing: there is
/// no runtime to report a state for, and `daemon.status` lists no accounts.
pub async fn start_account(
    runtimes: &Arc<RuntimeTable>,
    canonical: &Arc<CanonicalState>,
    cfg: AccountConfig,
    account_runtimes: bool,
) {
    use super::runtime::account::{AccountRuntime, Readiness};
    use super::runtime::pool::DEFAULT_READ_POOL_SIZE;

    if !account_runtimes {
        return;
    }
    let account = cfg.name.clone();
    let started =
        tokio::task::spawn_blocking(move || AccountRuntime::start(cfg, DEFAULT_READ_POOL_SIZE))
            .await;
    let change = match started {
        Ok(Ok(runtime)) => {
            let change = match runtime.readiness() {
                Readiness::Blocked { reason } => {
                    info!("[daemon] {account} is blocked: {reason}");
                    Change::AccountBlocked {
                        account: account.clone(),
                        reason,
                    }
                }
                _ => {
                    info!("[daemon] {account} is ready");
                    Change::AccountReady {
                        account: account.clone(),
                    }
                }
            };
            runtimes.insert(Arc::new(runtime));
            change
        }
        // A start that failed and a start whose thread died read the same way
        // to a client: nothing about the account can be served, and the reason
        // says which it was.
        Ok(Err(e)) => blocked_by_failure(runtimes, &account, format!("{e:#}")),
        Err(e) => blocked_by_failure(runtimes, &account, format!("the start task {e}")),
    };
    canonical.apply(change);
}

/// Record a start that never produced a runtime and build the change that says
/// so.
fn blocked_by_failure(runtimes: &RuntimeTable, account: &str, reason: String) -> Change {
    warn!("[daemon] could not start the runtime for {account}: {reason}");
    runtimes.insert_failure(account, reason.clone());
    Change::AccountBlocked {
        account: account.to_string(),
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> GlobalConfig {
        validate_document(text, Path::new("/c/config.toml")).expect("a valid document")
    }

    /// An omitted field reports the default the loader applied, not nothing,
    /// and both password fields are present and redacted.
    #[test]
    fn the_effective_config_carries_serde_defaults_and_no_secret() {
        let config = parse("[[accounts]]\nname = \"alpha\"\n");
        let json = effective_config(&config);
        assert_eq!(json["email"]["send_hold_secs"], json!(20));
        assert_eq!(json["secrets_backend"], json!("encrypted-file"));
        assert_eq!(json["accounts"][0]["smtp"]["port"], json!(465));
        assert_eq!(json["accounts"][0]["imap"]["password"], json!(REDACTED));
        assert_eq!(json["accounts"][0]["oauth2"], Value::Null);
        assert_eq!(json["accounts"][0]["mailboxes"]["extra"], json!([]));
    }

    /// A syntax error reports the 1-based line of the offending token; a
    /// semantic refusal has no span and reports none.
    #[test]
    fn a_diagnostic_has_a_line_only_when_the_parser_gave_one() {
        let broken = "[[accounts]]\nname = \"alpha\"\nnot a key = value = pair\n";
        let diagnostic = validate_document(broken, Path::new("/c/config.toml"))
            .expect_err("the document does not parse");
        assert_eq!(diagnostic.line, Some(3));

        let refused = validate_document(
            "[retention]\nmetadata_horizon_days = 40000\n",
            Path::new("/c/config.toml"),
        )
        .expect_err("the horizon is out of range");
        assert_eq!(refused.line, None);
        assert!(refused.message.contains("metadata_horizon_days"));
    }

    /// An account is updated when its effective JSON changed, and a global
    /// setting that resolves into it counts.
    #[test]
    fn the_three_lists_are_sorted_and_derived_from_the_effective_json() {
        let previous = parse("[[accounts]]\nname = \"beta\"\n\n[[accounts]]\nname = \"alpha\"\n");
        let next = parse(
            "[retention]\nbody_horizon_days = 30\n\n[[accounts]]\nname = \"beta\"\n\n\
             [[accounts]]\nname = \"gamma\"\n",
        );
        let plan = Reconcile::between(&previous, &next);
        assert_eq!(plan.added, vec!["gamma".to_string()]);
        assert_eq!(plan.updated, vec!["beta".to_string()]);
        assert_eq!(plan.removed, vec!["alpha".to_string()]);
    }

    /// A reformatted file is not a change.
    #[test]
    fn a_reformatted_document_reconciles_to_nothing() {
        let previous = parse("[[accounts]]\nname = \"alpha\"\n");
        let next = parse("# a comment\n\n[[accounts]]\n\nname = \"alpha\"\n\n");
        assert_eq!(Reconcile::between(&previous, &next), Reconcile::default());
    }
}
