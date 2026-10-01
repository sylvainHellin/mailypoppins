//! The Settings view's reads and writes of the daemon's configuration
//! (ACC-01, ACC-02, ACC-05, ACC-06, #0131): `config.get`, `config.reload`,
//! `config.set_password`, the account wizard's `config.init` and
//! `config.add_account`, and the device-code sign-in `config.oauth2_login`.
//!
//! There is no per-key writer (a tenth `config.*` method is pinned shut):
//! settings are edited in `config.toml` through the editor, then reloaded.
//! `config.get` is decoded leniently into [`EffectiveConfig`], which keeps
//! the handful of keys the view shows and defaults every one a daemon left
//! out, so a daemon that grows or drops a key never breaks the view.
//!
//! A reload the daemon refuses answers `-32007` after it published
//! `config.invalid` with the file, the line and the reason; the command
//! passes the refusal on as a protocol error carrying the daemon's own
//! sentence, and the event carries the line.
//!
//! A password crosses this layer once, from the webview to the daemon. It is
//! never in a `Debug` ([`SetPassword`]'s is hand-written, as the daemon's
//! `SetPasswordParams`), a `tracing` field, the fixture's journal or a
//! [`GuiError`]: a refusal names the account and the kind, never the value.
//!
//! The wizard's [`AccountDraft`] carries no secret at all: it serialises to
//! exactly the keys the daemon's `account_block` reads
//! (src/daemon/methods/config.rs), and a password follows as a second call
//! to `config.set_password` once the account exists, as the daemon's own
//! design asks (one path into the secrets backend).
//!
//! A sign-in is an operation the GUI awaits as `oauth2_login`. Its one
//! `operation.progress` has phase `device_code` and a message of two tokens,
//! the verification URL and the user code, split on their one space; it
//! settles as an [`OAuth2Stored`]. `operation_cancel` (`operation.cancel`)
//! settles it `cancelled`, but the daemon's provider poll runs on, so a sign-in finished in the
//! browser afterwards still caches its token.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::State;

use crate::calendar::daemon_sentence;
use crate::commands::{call, decode, with_door, OperationStarted};
use crate::error::{Addressing, GuiError};
use crate::session::{Door, PendingKind, SessionHandle};

/// What stands for a password wherever one would otherwise be printed, the
/// daemon's `REDACTED`.
pub const REDACTED: &str = "<redacted>";

/// `config.get` reads memory only.
const GET_BUDGET: Duration = Duration::from_secs(5);

/// A reload re-reads the file, then stops and starts every account whose
/// configuration changed and waits for each to settle.
const RELOAD_BUDGET: Duration = Duration::from_secs(60);

/// Storing one secret: the backend is opened on first use, and a keyring
/// may ask the user first.
const SECRET_BUDGET: Duration = Duration::from_secs(30);

/// Writing a configuration: validate, write, then start the new account's
/// runtime and wait for it to settle, as a reload does.
const WRITE_BUDGET: Duration = Duration::from_secs(60);

/// Starting a device-code sign-in: the answer is the operation id, before
/// the provider is asked anything.
const LOGIN_START_BUDGET: Duration = Duration::from_secs(10);

/// The phase of the one `operation.progress` a sign-in reports, the daemon's
/// `DEVICE_CODE_PHASE`.
pub const DEVICE_CODE_PHASE: &str = "device_code";

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/// `config.get`: the configuration the daemon serves, the revision it is at,
/// the file it was read from and whether that file loaded.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(default)]
pub struct ConfigSnapshot {
    /// The configuration revision: 0 at daemon start, one more per swap.
    pub revision: u64,
    /// The `config.toml` the daemon reads.
    pub path: String,
    /// `ok`, `absent` (no file yet) or `invalid` (the file exists and did
    /// not load when the daemon started).
    #[cfg_attr(test, ts(type = "\"ok\" | \"absent\" | \"invalid\""))]
    pub state: String,
    pub config: EffectiveConfig,
}

/// The part of the effective configuration the Settings view shows. Every
/// key is optional on the wire: a missing one is its default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(default)]
pub struct EffectiveConfig {
    /// `encrypted-file` or `keyring`.
    pub secrets_backend: String,
    pub email: ConfigEmail,
    pub accounts: Vec<ConfigAccount>,
}

/// `[email]`, the one key of it the view shows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(default)]
pub struct ConfigEmail {
    /// How long a send waits before it leaves, in seconds; 0 sends at once.
    pub send_hold_secs: u64,
}

/// One `[[accounts]]` entry as the view shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(default)]
pub struct ConfigAccount {
    pub name: String,
    pub default_from: String,
    /// `password`, `oauth2` or `graph`.
    pub auth_method: String,
    pub smtp: ConfigServer,
    pub imap: ConfigServer,
    /// The app registration of an `oauth2` or `graph` account.
    pub oauth2: Option<ConfigOAuth2>,
}

/// An SMTP or IMAP server; its password is never read, only redacted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(default)]
pub struct ConfigServer {
    pub host: String,
    pub port: u16,
    pub username: String,
}

/// An OAuth2 app registration: public identifiers, never redacted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(default)]
pub struct ConfigOAuth2 {
    pub client_id: String,
    pub tenant_id: String,
}

/// What a swap did: the accounts it started, restarted and stopped.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(default)]
pub struct ConfigSwap {
    pub added: Vec<String>,
    pub updated: Vec<String>,
    pub removed: Vec<String>,
}

/// Which password of an account.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(rename_all = "snake_case")]
pub enum SecretKind {
    Smtp,
    Imap,
}

impl SecretKind {
    /// The word it travels as.
    pub fn as_str(self) -> &'static str {
        match self {
            SecretKind::Smtp => "smtp",
            SecretKind::Imap => "imap",
        }
    }
}

/// `config.set_password`'s answer: where the secret went, never what it is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(default)]
pub struct SecretStored {
    pub stored: bool,
    pub account: String,
    /// `smtp` or `imap`.
    pub kind: String,
    /// The secrets backend's key.
    pub key: String,
}

/// How an account signs in, the daemon's `auth_method`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    Password,
    #[serde(rename = "oauth2")]
    OAuth2,
    Graph,
}

/// One account the wizard writes, in the shape `config.init` and
/// `config.add_account` take as their `account` parameter.
///
/// Every key is one the daemon's `account_block` reads, and an absent one is
/// left out of the block (the daemon's default then applies). It carries no
/// secret: a password is stored after the account exists, through
/// `config.set_password`. Unknown keys from the webview are refused rather
/// than dropped, so a draft that grew a field the daemon would ignore fails
/// loudly here.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(deny_unknown_fields)]
pub struct AccountDraft {
    /// The account's slug, unique in the file.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub default_from: Option<String>,
    /// Absent for a password account, the daemon's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub auth_method: Option<AuthMethod>,
    /// The app registration of an `oauth2` or `graph` account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub oauth2: Option<ConfigOAuth2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub smtp: Option<AccountDraftServer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub imap: Option<AccountDraftServer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub mailboxes: Option<AccountDraftMailboxes>,
}

/// An SMTP or IMAP server of an [`AccountDraft`]; no password key exists.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(deny_unknown_fields)]
pub struct AccountDraftServer {
    /// Absent on IMAP, the SMTP host is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub port: Option<u16>,
    /// Absent on IMAP, the SMTP username is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub username: Option<String>,
    /// Proton Bridge's self-signed certificate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub accept_invalid_certs: Option<bool>,
}

/// The server names of the three standard mailboxes, and any extra ones.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(deny_unknown_fields)]
pub struct AccountDraftMailboxes {
    pub inbox: String,
    pub archive: String,
    pub sent: String,
    /// Omitted on the wire when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra: Vec<String>,
}

/// `config.init`'s answer: the swap that started the first account, and the
/// file it wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct ConfigInitialised {
    /// The `config.toml` written.
    #[serde(default)]
    pub path: String,
    #[serde(flatten)]
    #[cfg_attr(test, ts(flatten))]
    pub swap: ConfigSwap,
}

/// What a finished sign-in settles with: where the token went, never what
/// it is. `kind` is `oauth2` (IMAP/SMTP) or `graph`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(default)]
pub struct OAuth2Stored {
    pub stored: bool,
    pub account: String,
    pub kind: String,
    /// The token cache's key, `oauth2-token-<account>`.
    pub key: String,
}

/// One password on its way to the daemon.
///
/// `Debug` is hand-written and prints [`REDACTED`] where the value is, as
/// the daemon's `SetPasswordParams` does: a derived one would print it the
/// day someone logs the struct.
#[derive(Clone)]
pub struct SetPassword {
    pub account: String,
    pub kind: SecretKind,
    value: String,
}

impl SetPassword {
    pub fn new(account: impl Into<String>, kind: SecretKind, value: impl Into<String>) -> Self {
        SetPassword {
            account: account.into(),
            kind,
            value: value.into(),
        }
    }
}

impl std::fmt::Debug for SetPassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetPassword")
            .field("account", &self.account)
            .field("kind", &self.kind)
            .field("value", &REDACTED)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// The calls
// ---------------------------------------------------------------------------

/// The daemon's configuration, whatever its state.
pub fn config_get_on(door: &Door) -> Result<ConfigSnapshot, GuiError> {
    let answer = call(
        door,
        "config.get",
        json!({}),
        GET_BUDGET,
        Addressing::Resource,
    )?;
    decode("config.get", answer)
}

/// Re-read `config.toml` and swap it in. A file that does not load is a
/// protocol error with the daemon's sentence (code `-32007`); the daemon
/// keeps serving what it had, and its `config.invalid` names the line.
pub fn config_reload_on(door: &Door) -> Result<ConfigSwap, GuiError> {
    let answer = call(
        door,
        "config.reload",
        json!({}),
        RELOAD_BUDGET,
        Addressing::Params,
    )
    .map_err(daemon_sentence)?;
    decode("config.reload", answer)
}

/// Store one password through the daemon's secrets backend.
pub fn config_set_password_on(
    door: &Door,
    password: &SetPassword,
) -> Result<SecretStored, GuiError> {
    let kind = password.kind.as_str();
    let account = password.account.as_str();
    let answer = call(
        door,
        "config.set_password",
        json!({"account": account, "kind": kind, "value": password.value}),
        SECRET_BUDGET,
        Addressing::Params,
    )
    .map_err(daemon_sentence);
    let answer = match answer {
        Ok(answer) => answer,
        Err(e) => {
            tracing::warn!("[config] storing the {kind} password of {account} failed: {e}");
            return Err(e);
        }
    };
    let stored: SecretStored = decode("config.set_password", answer)?;
    tracing::info!("[config] stored the {kind} password of {account}");
    Ok(stored)
}

/// Append one account to `config.toml`. The daemon refuses when no file
/// exists yet (then [`config_init_on`]) or the name is taken, with its own
/// sentence; a candidate that does not load is `-32007` and the file stays
/// as it was.
pub fn config_add_account_on(door: &Door, account: &AccountDraft) -> Result<ConfigSwap, GuiError> {
    let wire = serde_json::to_value(account).map_err(GuiError::internal)?;
    let answer = call(
        door,
        "config.add_account",
        json!({"account": wire}),
        WRITE_BUDGET,
        Addressing::Params,
    )
    .map_err(daemon_sentence)?;
    let swap: ConfigSwap = decode("config.add_account", answer)?;
    tracing::info!("[config] added the account {}", account.name);
    Ok(swap)
}

/// Write the first `config.toml`, holding one account. The daemon refuses
/// when a file exists already.
pub fn config_init_on(door: &Door, account: &AccountDraft) -> Result<ConfigInitialised, GuiError> {
    let wire = serde_json::to_value(account).map_err(GuiError::internal)?;
    let answer = call(
        door,
        "config.init",
        json!({"account": wire}),
        WRITE_BUDGET,
        Addressing::Params,
    )
    .map_err(daemon_sentence)?;
    let written: ConfigInitialised = decode("config.init", answer)?;
    tracing::info!(
        "[config] wrote {} with the account {}",
        written.path,
        account.name
    );
    Ok(written)
}

/// Start the device-code sign-in of `account`, awaited as `oauth2_login`.
/// The daemon refuses an unknown account, a password account and one with
/// no client or tenant, each with its own sentence.
pub fn config_oauth2_login_on(
    session: &SessionHandle,
    door: &Door,
    account: &str,
) -> Result<OperationStarted, GuiError> {
    let operation_id = session
        .start_operation(
            door,
            "config.oauth2_login",
            json!({"account": account}),
            PendingKind::OAuth2Login,
            LOGIN_START_BUDGET,
        )
        .map_err(daemon_sentence)?;
    tracing::info!("[config] sign-in of {account} started as {operation_id}");
    Ok(OperationStarted { operation_id })
}

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

/// Append one account (the wizard's Add account).
#[tauri::command(rename_all = "snake_case")]
pub async fn config_add_account(
    session: State<'_, SessionHandle>,
    account: AccountDraft,
) -> Result<ConfigSwap, GuiError> {
    with_door(&session, move |_, door| {
        config_add_account_on(door, &account)
    })
    .await
}

/// Write the first configuration (the first-run wizard).
#[tauri::command(rename_all = "snake_case")]
pub async fn config_init(
    session: State<'_, SessionHandle>,
    account: AccountDraft,
) -> Result<ConfigInitialised, GuiError> {
    with_door(&session, move |_, door| config_init_on(door, &account)).await
}

/// Start a device-code sign-in, awaited as `oauth2_login`.
#[tauri::command(rename_all = "snake_case")]
pub async fn config_oauth2_login(
    session: State<'_, SessionHandle>,
    account: String,
) -> Result<OperationStarted, GuiError> {
    with_door(&session, move |session, door| {
        config_oauth2_login_on(session, door, &account)
    })
    .await
}

/// The daemon's configuration, for the Settings view.
#[tauri::command(rename_all = "snake_case")]
pub async fn config_get(session: State<'_, SessionHandle>) -> Result<ConfigSnapshot, GuiError> {
    with_door(&session, |_, door| config_get_on(door)).await
}

/// Reload `config.toml`; `-32007` when it does not load.
#[tauri::command(rename_all = "snake_case")]
pub async fn config_reload(session: State<'_, SessionHandle>) -> Result<ConfigSwap, GuiError> {
    with_door(&session, |_, door| config_reload_on(door)).await
}

/// Store the SMTP or IMAP password of `account`.
#[tauri::command(rename_all = "snake_case")]
pub async fn config_set_password(
    session: State<'_, SessionHandle>,
    account: String,
    kind: SecretKind,
    value: String,
) -> Result<SecretStored, GuiError> {
    let password = SetPassword::new(account, kind, value);
    with_door(&session, move |_, door| {
        config_set_password_on(door, &password)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{Fixture, CONFIG_INVALID_LINE, CONFIG_INVALID_MESSAGE};
    use mp_client::events::Incoming;
    use std::io::Write;
    use std::sync::mpsc::{channel, Receiver};
    use std::sync::{Arc, Mutex};

    const SECRET: &str = "hunter2-correct-horse";

    fn fixture_door() -> (Door, Arc<Fixture>, Receiver<Incoming>) {
        let (tx, rx) = channel();
        let fixture = Arc::new(Fixture::load(tx).expect("fixture"));
        (Door::Fixture(Arc::clone(&fixture)), fixture, rx)
    }

    fn events(rx: &Receiver<Incoming>) -> Vec<(String, serde_json::Value)> {
        rx.try_iter()
            .filter_map(|i| match i {
                Incoming::Event(e) => Some((e.kind, e.payload)),
                _ => None,
            })
            .collect()
    }

    /// A `tracing` writer that keeps every line.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if let Ok(mut b) = self.0.lock() {
                b.extend_from_slice(buf);
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Captured {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().map(|b| b.clone()).unwrap_or_default())
                .into_owned()
        }
    }

    /// Run `f` under a subscriber that records every level, and answer what it wrote.
    fn traced<T>(f: impl FnOnce() -> T) -> (T, String) {
        let captured = Captured::default();
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let out = tracing::subscriber::with_default(subscriber, f);
        (out, captured.text())
    }

    /// The keys the daemon's `account_block` reads (src/daemon/methods/config.rs),
    /// as paths: a draft key outside these would be dropped without a word.
    const ACCOUNT_BLOCK_KEYS: &[&str] = &[
        "name",
        "default_from",
        "auth_method",
        "save_to_sent",
        "oauth2",
        "oauth2.client_id",
        "oauth2.tenant_id",
        "smtp",
        "imap",
        "smtp.host",
        "smtp.username",
        "smtp.port",
        "smtp.fetch_concurrency",
        "smtp.body_fetch_deadline_secs",
        "smtp.sync_interval_secs",
        "smtp.accept_invalid_certs",
        "imap.host",
        "imap.username",
        "imap.port",
        "imap.fetch_concurrency",
        "imap.body_fetch_deadline_secs",
        "imap.sync_interval_secs",
        "imap.accept_invalid_certs",
        "mailboxes",
        "mailboxes.inbox",
        "mailboxes.archive",
        "mailboxes.sent",
        "mailboxes.extra",
    ];

    /// Every key path of an object, nested objects included.
    fn key_paths(value: &serde_json::Value, prefix: &str, out: &mut Vec<String>) {
        if let Some(object) = value.as_object() {
            for (key, inner) in object {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                out.push(path.clone());
                key_paths(inner, &path, out);
            }
        }
    }

    fn server(host: &str, port: u16, username: &str, invalid_certs: bool) -> AccountDraftServer {
        AccountDraftServer {
            host: Some(host.to_string()).filter(|h| !h.is_empty()),
            port: Some(port),
            username: Some(username.to_string()).filter(|u| !u.is_empty()),
            accept_invalid_certs: invalid_certs.then_some(true),
        }
    }

    fn standard_mailboxes(sent: &str) -> AccountDraftMailboxes {
        AccountDraftMailboxes {
            inbox: "INBOX".into(),
            archive: "Archive".into(),
            sent: sent.into(),
            extra: Vec::new(),
        }
    }

    /// The wizard's four presets (src/config_cmd/init.rs:80-146), filled in.
    fn presets() -> Vec<(&'static str, AccountDraft)> {
        let app = || ConfigOAuth2 {
            client_id: "00000000-0000-0000-0000-000000000001".into(),
            tenant_id: "contoso.onmicrosoft.com".into(),
        };
        vec![
            (
                "imap",
                AccountDraft {
                    name: "main".into(),
                    default_from: Some("Me <me@example.com>".into()),
                    smtp: Some(server("smtp.example.com", 465, "me@example.com", false)),
                    imap: Some(server("imap.example.com", 993, "", false)),
                    mailboxes: Some(standard_mailboxes("Sent")),
                    ..Default::default()
                },
            ),
            (
                "proton",
                AccountDraft {
                    name: "proton".into(),
                    default_from: Some("me@proton.me".into()),
                    smtp: Some(server("127.0.0.1", 1025, "me@proton.me", true)),
                    imap: Some(server("127.0.0.1", 1143, "", true)),
                    mailboxes: Some(AccountDraftMailboxes {
                        extra: vec!["Folders/Invoices".into()],
                        ..standard_mailboxes("Sent")
                    }),
                    ..Default::default()
                },
            ),
            (
                "microsoft365",
                AccountDraft {
                    name: "exchange".into(),
                    default_from: Some("me@contoso.com".into()),
                    auth_method: Some(AuthMethod::OAuth2),
                    oauth2: Some(app()),
                    smtp: Some(server("smtp.office365.com", 587, "me@contoso.com", false)),
                    imap: Some(server("outlook.office365.com", 993, "", false)),
                    mailboxes: Some(standard_mailboxes("Sent")),
                },
            ),
            (
                "graph",
                AccountDraft {
                    name: "work".into(),
                    default_from: Some("me@contoso.com".into()),
                    auth_method: Some(AuthMethod::Graph),
                    oauth2: Some(app()),
                    mailboxes: Some(AccountDraftMailboxes {
                        inbox: "Inbox".into(),
                        archive: "Archive".into(),
                        sent: "Sent Items".into(),
                        extra: Vec::new(),
                    }),
                    ..Default::default()
                },
            ),
        ]
    }

    #[test]
    fn every_presets_draft_serialises_to_account_block_keys_and_nothing_else() {
        for (preset, draft) in presets() {
            let wire = serde_json::to_value(&draft).expect("serialises");
            let mut paths = Vec::new();
            key_paths(&wire, "", &mut paths);
            for path in &paths {
                assert!(
                    ACCOUNT_BLOCK_KEYS.contains(&path.as_str()),
                    "{preset}: {path} is not a key account_block reads"
                );
                assert!(!path.contains("password"), "{preset}: {path}");
            }
            // What the webview sends comes back the same.
            let back: AccountDraft = serde_json::from_value(wire.clone()).expect("decodes");
            assert_eq!(back, draft, "{preset}");
            let text = wire.to_string();
            assert!(
                !text.contains("null"),
                "{preset}: an absent key is left out: {text}"
            );
        }
        let graph = serde_json::to_value(&presets()[3].1).expect("graph");
        assert_eq!(graph["auth_method"], "graph");
        assert!(graph.get("smtp").is_none() && graph.get("imap").is_none());
        let oauth2 = serde_json::to_value(&presets()[2].1).expect("oauth2");
        assert_eq!(oauth2["auth_method"], "oauth2");
        assert_eq!(oauth2["oauth2"]["tenant_id"], "contoso.onmicrosoft.com");
        let proton = serde_json::to_value(&presets()[1].1).expect("proton");
        assert_eq!(proton["smtp"]["accept_invalid_certs"], true);
        assert_eq!(proton["mailboxes"]["extra"], json!(["Folders/Invoices"]));
        let imap = serde_json::to_value(&presets()[0].1).expect("imap");
        assert!(
            imap.get("auth_method").is_none(),
            "a password account leaves it to the default"
        );
        assert!(
            imap["imap"].get("username").is_none(),
            "empty uses the SMTP username"
        );
    }

    #[test]
    fn a_draft_with_a_password_or_any_unknown_key_is_refused() {
        for bad in [
            json!({"name": "x", "password": "hunter2"}),
            json!({"name": "x", "smtp": {"host": "h", "password": "hunter2"}}),
            json!({"name": "x", "mailboxes": {"inbox": "INBOX", "archive": "A", "sent": "S", "drafts": "D"}}),
        ] {
            assert!(
                serde_json::from_value::<AccountDraft>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn config_add_account_appends_the_block_and_serves_a_ready_account() {
        let (door, f, rx) = fixture_door();
        let (_, draft) = presets().remove(1);
        let swap = config_add_account_on(&door, &draft).expect("added");
        assert_eq!(
            swap,
            ConfigSwap {
                added: vec!["proton".into()],
                ..Default::default()
            }
        );
        let got = events(&rx);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].0, "config.changed");
        assert_eq!(
            got[0].1,
            json!({"added": ["proton"], "updated": [], "removed": [], "config_revision": 1})
        );
        let text = std::fs::read_to_string(f.root().join("config.toml")).expect("config.toml");
        assert!(
            text.contains("\n[[accounts]]\nname = \"proton\"\ndefault_from = \"me@proton.me\"\n"),
            "{text}"
        );
        assert!(text.contains("[accounts.smtp]\nhost = \"127.0.0.1\"\nusername = \"me@proton.me\"\nport = 1025\naccept_invalid_certs = true\n"), "{text}");
        assert!(
            text.contains("[[accounts.mailboxes.extra]]\nserver = \"Folders/Invoices\"\n"),
            "{text}"
        );
        assert!(!text.contains("password"), "{text}");

        let names: Vec<String> = f.call("account.list", json!({})).expect("account.list")
            ["accounts"]
            .as_array()
            .expect("accounts")
            .iter()
            .map(|a| {
                format!(
                    "{}:{}",
                    a["name"].as_str().unwrap_or(""),
                    a["state"].as_str().unwrap_or("")
                )
            })
            .collect();
        assert_eq!(names, ["work:ready", "home:ready", "proton:ready"]);
        let boxes = f
            .call("mailbox.list", json!({"account": "proton"}))
            .expect("mailbox.list");
        let slugs: Vec<&str> = boxes["mailboxes"]
            .as_array()
            .expect("mailboxes")
            .iter()
            .map(|m| m["slug"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(slugs, ["inbox", "archive", "sent"]);
        let snapshot = config_get_on(&door).expect("get");
        assert_eq!(snapshot.revision, 1);
        let added = snapshot.config.accounts.last().expect("the new account");
        assert_eq!(added.name, "proton");
        assert_eq!(added.smtp.port, 1025);
        assert_eq!(added.auth_method, "password");

        let err = config_add_account_on(&door, &draft).expect_err("taken");
        assert_eq!(
            err.message(),
            "an account named proton is already configured"
        );
    }

    #[test]
    fn config_init_is_refused_while_a_configuration_exists() {
        let (door, f, _rx) = fixture_door();
        let (_, draft) = presets().remove(0);
        let err = config_init_on(&door, &draft).expect_err("a file exists");
        let path = f.root().join("config.toml");
        assert_eq!(
            err.message(),
            format!(
                "a configuration already exists at {}; edit it and call config.reload",
                path.display()
            )
        );
        assert_eq!(config_get_on(&door).expect("get").config.accounts.len(), 2);
    }

    #[test]
    fn config_init_after_config_absent_writes_the_first_configuration() {
        let (door, f, rx) = fixture_door();
        f.simulate("config_absent").expect("absent");
        let path = f.root().join("config.toml");
        assert!(!path.exists());
        let snapshot = config_get_on(&door).expect("get");
        assert_eq!(snapshot.state, "absent");
        assert!(snapshot.config.accounts.is_empty());
        let bootstrap = f.call("state.bootstrap", json!({})).expect("bootstrap");
        assert_eq!(bootstrap["snapshot"]["accounts"], json!([]));
        let (_, draft) = presets().remove(3);
        let err = config_add_account_on(&door, &draft).expect_err("no file yet");
        assert_eq!(
            err.message(),
            format!(
                "there is no configuration at {}; write one with config.init first",
                path.display()
            )
        );
        while rx.try_recv().is_ok() {}

        let written = config_init_on(&door, &draft).expect("written");
        assert_eq!(written.path, path.display().to_string());
        assert_eq!(written.swap.added, ["work"]);
        let got = events(&rx);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].0, "config.changed");
        let text = std::fs::read_to_string(&path).expect("config.toml");
        assert!(
            text.contains("auth_method = \"graph\"\n\n[accounts.oauth2]\n"),
            "{text}"
        );
        assert!(!text.contains("[accounts.smtp]"), "{text}");
        let snapshot = config_get_on(&door).expect("get");
        assert_eq!(snapshot.state, "ok");
        assert_eq!(snapshot.config.accounts[0].auth_method, "graph");
        let accounts = f.call("account.list", json!({})).expect("account.list");
        assert_eq!(
            accounts["accounts"],
            json!([{"name": "work", "default": true, "backend": "graph", "state": "ready"}])
        );
        assert!(config_init_on(&door, &draft).is_err(), "only once");
    }

    #[test]
    fn a_sign_in_is_refused_with_the_daemons_sentences() {
        let session = SessionHandle::new(true);
        let (door, _f, _rx) = fixture_door();
        let err = config_oauth2_login_on(&session, &door, "nobody").expect_err("unknown");
        assert_eq!(err.message(), "Account 'nobody' not found in config");
        let err = config_oauth2_login_on(&session, &door, "work").expect_err("password");
        assert!(
            err.message()
                .starts_with("Account 'work' uses auth_method = \"password\""),
            "{}",
            err.message()
        );
        assert!(session.pending().is_empty());
        let started = config_oauth2_login_on(&session, &door, "home").expect("graph signs in");
        assert_eq!(
            session.pending_kind(&started.operation_id),
            Some(PendingKind::OAuth2Login)
        );
    }

    #[test]
    fn config_get_decodes_the_fixture_configuration() {
        let (door, f, _rx) = fixture_door();
        let snapshot = config_get_on(&door).expect("config.get");
        assert_eq!(snapshot.state, "ok");
        assert_eq!(
            snapshot.path,
            f.root().join("config.toml").display().to_string()
        );
        assert_eq!(snapshot.config.secrets_backend, "encrypted-file");
        assert_eq!(snapshot.config.email.send_hold_secs, 10);
        let names: Vec<&str> = snapshot
            .config
            .accounts
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        assert_eq!(names, ["work", "home"]);
        let work = &snapshot.config.accounts[0];
        assert_eq!(work.auth_method, "password");
        assert_eq!(work.smtp.host, "smtp.work.example");
        assert_eq!(work.smtp.port, 465);
        assert_eq!(work.imap.host, "imap.work.example");
        assert_eq!(work.oauth2, None);
        let home = &snapshot.config.accounts[1];
        assert_eq!(home.auth_method, "graph");
        assert_eq!(
            home.oauth2.as_ref().map(|o| o.tenant_id.as_str()),
            Some("common")
        );
    }

    #[test]
    fn the_effective_configuration_tolerates_missing_and_extra_keys() {
        let snapshot: ConfigSnapshot = serde_json::from_value(json!({
            "path": "/c/config.toml",
            "config": {
                "theme": "dark",
                "accounts": [{"name": "bare"}, {"name": "half", "smtp": {"host": "h"}, "oauth2": null}],
            },
        }))
        .expect("lenient");
        assert_eq!(snapshot.revision, 0);
        assert_eq!(snapshot.state, "");
        assert_eq!(snapshot.config.secrets_backend, "");
        assert_eq!(snapshot.config.email.send_hold_secs, 0);
        assert_eq!(
            snapshot.config.accounts[0],
            ConfigAccount {
                name: "bare".into(),
                ..Default::default()
            }
        );
        assert_eq!(snapshot.config.accounts[1].smtp.host, "h");
        assert_eq!(snapshot.config.accounts[1].smtp.port, 0);
        assert_eq!(snapshot.config.accounts[1].oauth2, None);
        let empty: ConfigSnapshot = serde_json::from_value(json!({})).expect("empty");
        assert_eq!(empty, ConfigSnapshot::default());
    }

    #[test]
    fn a_reload_of_a_good_file_answers_the_swap_and_publishes_config_changed() {
        let (door, _f, rx) = fixture_door();
        let swap = config_reload_on(&door).expect("reloaded");
        assert_eq!(swap, ConfigSwap::default());
        let got = events(&rx);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].0, "config.changed");
        assert_eq!(got[0].1["config_revision"], 1);
        assert_eq!(config_get_on(&door).expect("get").revision, 1);
    }

    #[test]
    fn a_reload_of_an_invalid_file_is_a_protocol_error_with_the_daemons_sentence() {
        let (door, f, rx) = fixture_door();
        f.simulate("config_invalid").expect("broken");
        let path = f.root().join("config.toml");
        let text = std::fs::read_to_string(&path).expect("config.toml");
        let line = text
            .lines()
            .position(|l| l == CONFIG_INVALID_LINE)
            .expect("the broken line")
            + 1;
        let err = config_reload_on(&door).expect_err("refused");
        assert_eq!(
            err,
            GuiError::Protocol {
                message: CONFIG_INVALID_MESSAGE.to_string(),
                code: Some(-32007),
            }
        );
        let got = events(&rx);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].0, "config.invalid");
        assert_eq!(
            got[0].1,
            json!({"path": path.display().to_string(), "line": line, "message": CONFIG_INVALID_MESSAGE})
        );
        // The daemon keeps serving what it had: the state and revision stay.
        let snapshot = config_get_on(&door).expect("get");
        assert_eq!((snapshot.state.as_str(), snapshot.revision), ("ok", 0));
    }

    #[test]
    fn a_stored_password_is_in_no_journal_no_debug_and_no_tracing_line() {
        let (door, f, _rx) = fixture_door();
        let password = SetPassword::new("work", SecretKind::Imap, SECRET);
        let debug = format!("{password:?} {password:#?}");
        assert!(!debug.contains(SECRET), "{debug}");
        assert!(debug.contains(REDACTED), "{debug}");

        let (stored, lines) = traced(|| config_set_password_on(&door, &password));
        let stored = stored.expect("stored");
        assert_eq!(
            stored,
            SecretStored {
                stored: true,
                account: "work".into(),
                kind: "imap".into(),
                key: "imap-password-work".into(),
            }
        );
        assert!(
            lines.contains("stored the imap password of work"),
            "{lines}"
        );
        assert!(!lines.contains(SECRET), "{lines}");

        assert_eq!(
            f.password_writes(),
            vec![json!({"account": "work", "kind": "imap", "value": REDACTED})]
        );
        let calls = format!("{:?}", f.calls());
        assert!(calls.contains("config.set_password"), "{calls}");
        assert!(!calls.contains(SECRET), "{calls}");
    }

    #[test]
    fn a_refused_password_names_the_account_and_never_the_value() {
        let (door, f, _rx) = fixture_door();
        let password = SetPassword::new("nobody", SecretKind::Smtp, SECRET);
        let (err, lines) = traced(|| config_set_password_on(&door, &password));
        let err = err.expect_err("unknown account");
        assert!(
            matches!(
                err,
                GuiError::NotFound {
                    code: Some(-32005),
                    ..
                }
            ),
            "{err:?}"
        );
        assert_eq!(err.message(), "no account named nobody is configured");
        let serialised = serde_json::to_string(&err).expect("serialises");
        assert!(!serialised.contains(SECRET), "{serialised}");
        assert!(!format!("{err:?}").contains(SECRET));
        assert!(
            lines.contains("storing the smtp password of nobody failed"),
            "{lines}"
        );
        assert!(!lines.contains(SECRET), "{lines}");
        assert!(f.password_writes().is_empty());
    }
}
