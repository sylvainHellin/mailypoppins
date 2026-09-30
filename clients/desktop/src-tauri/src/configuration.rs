//! The Settings view's reads and writes of the daemon's configuration
//! (ACC-05, #0131): `config.get`, `config.reload` and `config.set_password`.
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

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::State;

use crate::calendar::daemon_sentence;
use crate::commands::{call, decode, with_door};
use crate::error::{Addressing, GuiError};
use crate::session::{Door, SessionHandle};

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

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

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
