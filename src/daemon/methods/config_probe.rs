//! `config.probe`: dial the servers of an account that is not configured yet.
//!
//! ```text
//! config.probe {account, check, password?} -> {ok: true, mailboxes?} | {ok: false, error}
//! ```
//!
//! The setup wizards (`mp config init`, `mp config add-account`) test the
//! connection the user has just described before they write anything, and
//! offer the server's mailboxes as the choices of the next prompt. Both need a
//! session on a mail server, which is the daemon's to open, for an account
//! `config.toml` does not hold yet, which no other method takes. So the
//! candidate travels: `account` is the `[[accounts]]` block `config.init` and
//! `config.add_account` take, read through the same loader, so a probe and the
//! write that follows it agree about every default.
//!
//! `check` is one of four:
//!
//! - `smtp`: connect, authenticate, hang up, send nothing.
//! - `imap`: log in and log out.
//! - `graph`: acquire the Graph client, which refreshes the cached token.
//! - `mailboxes`: the server's mailbox names (IMAP `LIST`) or the account's
//!   folder names (Graph), in the server's order.
//!
//! A password account sends `password` for `smtp` and `imap`; an `oauth2` or
//! `graph` account is probed with the token cached under its name, so a wizard
//! signs in (`config.oauth2_login` with a `candidate`) first. The password is
//! used for the one session and dropped: it is never stored, logged or echoed,
//! and [`ProbeParams`]'s `Debug` prints [`REDACTED`] for it.
//!
//! A server that refuses is an answer, `{ok: false, error}`, not a refusal of
//! the call: the wizard prints it and asks again. A query, because it writes
//! nothing here or on the server, as `mailbox.list_server` is one.

use serde_json::{json, Value};

use crate::config::{AccountConfig, AuthMethod, GraphConfig, ImapConfig, SmtpConfig};

use super::super::dispatch::DomainError;
use super::config::{candidate_account, REDACTED};

/// The four things a probe can check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// The SMTP login.
    Smtp,
    /// The IMAP login.
    Imap,
    /// The Graph client.
    Graph,
    /// The server's mailbox or folder names.
    Mailboxes,
}

impl Check {
    /// The check a wire word names.
    pub fn from_wire(word: &str) -> Option<Check> {
        match word {
            "smtp" => Some(Check::Smtp),
            "imap" => Some(Check::Imap),
            "graph" => Some(Check::Graph),
            "mailboxes" => Some(Check::Mailboxes),
            _ => None,
        }
    }
}

/// The parameters of one probe, with a `Debug` that never prints the password.
pub struct ProbeParams {
    account: AccountConfig,
    check: Check,
    password: Option<String>,
}

impl std::fmt::Debug for ProbeParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProbeParams")
            .field("account", &self.account.name)
            .field("check", &self.check)
            .field("password", &self.password.as_ref().map(|_| REDACTED))
            .finish()
    }
}

impl ProbeParams {
    /// The parameters off the wire, or `-32602` naming what is wrong and
    /// echoing no value.
    pub fn from_params(params: &Value) -> Result<ProbeParams, DomainError> {
        super::only_params("config.probe", params, &["account", "check", "password"])?;
        let candidate = params
            .get("account")
            .filter(|value| value.is_object())
            .ok_or_else(|| DomainError::invalid_params("account is a required object parameter"))?;
        let account = candidate_account(candidate)?;
        let check = params
            .get("check")
            .and_then(Value::as_str)
            .and_then(Check::from_wire)
            .ok_or_else(|| {
                DomainError::invalid_params(
                    "check is one of \"smtp\", \"imap\", \"graph\", \"mailboxes\"",
                )
            })?;
        let password = match params.get("password") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) => Some(value.clone()),
            Some(_) => {
                return Err(DomainError::invalid_params(
                    "password is a string parameter",
                ))
            }
        };
        let graph = account.auth_method == AuthMethod::Graph;
        match check {
            Check::Smtp | Check::Imap if graph => {
                return Err(DomainError::invalid_params(format!(
                    "{} has auth_method = \"graph\" and no SMTP or IMAP server to test",
                    account.name
                )))
            }
            Check::Graph if !graph => {
                return Err(DomainError::invalid_params(format!(
                    "{} does not use auth_method = \"graph\"",
                    account.name
                )))
            }
            _ => {}
        }
        if account.auth_method == AuthMethod::Password
            && matches!(check, Check::Smtp | Check::Imap | Check::Mailboxes)
            && password.is_none()
        {
            return Err(DomainError::invalid_params(
                "a password account is probed with its password",
            ));
        }
        Ok(ProbeParams {
            account,
            check,
            password,
        })
    }
}

/// Run one probe and answer what it found.
pub async fn probe(params: ProbeParams) -> Value {
    let account = &params.account;
    let outcome: anyhow::Result<Value> = async {
        match params.check {
            Check::Smtp => {
                let smtp = smtp_config(account, secret(&params)?);
                crate::send::test_smtp_connection(&smtp).await?;
                Ok(json!({"ok": true}))
            }
            Check::Imap => {
                let imap = imap_config(account, secret(&params)?);
                let mut session = crate::imap_client::open_imap_session(&imap).await?;
                session.logout().await.ok();
                Ok(json!({"ok": true}))
            }
            Check::Graph => {
                crate::graph::GraphClient::new_async(&graph_config(account)).await?;
                Ok(json!({"ok": true}))
            }
            Check::Mailboxes if account.auth_method == AuthMethod::Graph => {
                let client = crate::graph::GraphClient::new_async(&graph_config(account)).await?;
                let folders = client.list_folders().await?;
                let names: Vec<String> = folders.into_iter().map(|f| f.display_name).collect();
                Ok(json!({"ok": true, "mailboxes": names}))
            }
            Check::Mailboxes => {
                let imap = imap_config(account, secret(&params)?);
                let names = crate::imap_client::list_mailboxes(&imap).await?;
                Ok(json!({"ok": true, "mailboxes": names}))
            }
        }
    }
    .await;
    match outcome {
        Ok(answer) => answer,
        Err(e) => {
            log::info!(
                "[daemon] config.probe of {} ({:?}) failed: {e:#}",
                account.name,
                params.check
            );
            json!({"ok": false, "error": format!("{e:#}")})
        }
    }
}

/// What the session authenticates with: the password the wizard sent for a
/// password account, the cached access token for an OAuth2 one.
fn secret(params: &ProbeParams) -> anyhow::Result<String> {
    match params.account.auth_method {
        AuthMethod::Password => Ok(params.password.clone().unwrap_or_default()),
        _ => crate::oauth2::load_token_cache(&params.account.name)
            .map(|cache| cache.access_token)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no OAuth2 token is cached for {}; sign in first",
                    params.account.name
                )
            }),
    }
}

/// The SMTP settings of the candidate.
fn smtp_config(account: &AccountConfig, password: String) -> SmtpConfig {
    SmtpConfig {
        host: account.smtp.host.clone(),
        port: account.smtp.port,
        username: account.smtp.username.clone(),
        password,
        default_from: account.default_from.clone(),
        accept_invalid_certs: account.smtp.accept_invalid_certs,
        auth_method: account.auth_method.clone(),
    }
}

/// The IMAP settings of the candidate, an empty host or username falling back
/// to the SMTP one as `ImapConfig::load` does.
fn imap_config(account: &AccountConfig, password: String) -> ImapConfig {
    let or_smtp = |imap: &str, smtp: &str| {
        if imap.is_empty() {
            smtp.to_string()
        } else {
            imap.to_string()
        }
    };
    ImapConfig {
        host: or_smtp(&account.imap.host, &account.smtp.host),
        port: account.imap.port,
        username: or_smtp(&account.imap.username, &account.smtp.username),
        password,
        accept_invalid_certs: account.imap.accept_invalid_certs,
        auth_method: account.auth_method.clone(),
        fetch_concurrency: 1,
        body_fetch_deadline_secs: 30,
    }
}

/// The Graph settings of the candidate; the token is the one cached under its
/// name.
fn graph_config(account: &AccountConfig) -> GraphConfig {
    let oauth2 = account.oauth2.clone().unwrap_or_default();
    GraphConfig {
        client_id: oauth2.client_id,
        tenant_id: oauth2.tenant_id,
        username: account.default_from.clone(),
        account_name: account.name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn password_account() -> Value {
        json!({
            "name": "work",
            "default_from": "me@example.com",
            "smtp": {"host": "smtp.example.com", "port": 465, "username": "me@example.com"},
            "imap": {"port": 993},
        })
    }

    /// The password reaches the probe and nothing that prints it.
    #[test]
    fn the_debug_of_the_params_never_prints_the_password() {
        let params = ProbeParams::from_params(&json!({
            "account": password_account(),
            "check": "imap",
            "password": "hunter2",
        }))
        .expect("valid");
        let debug = format!("{params:?}");
        assert!(!debug.contains("hunter2"), "{debug}");
        assert!(debug.contains(REDACTED), "{debug}");
    }

    /// An empty IMAP host and username fall back to the SMTP ones, and the
    /// loader's defaults apply to the candidate as they will to the file.
    #[test]
    fn the_candidate_reads_through_the_loader_with_its_fallbacks() {
        let params = ProbeParams::from_params(&json!({
            "account": password_account(),
            "check": "imap",
            "password": "pw",
        }))
        .expect("valid");
        let imap = imap_config(&params.account, "pw".into());
        assert_eq!(imap.host, "smtp.example.com");
        assert_eq!(imap.username, "me@example.com");
        assert_eq!(imap.port, 993);
    }

    /// The refusals: an unknown check, a password account with no password,
    /// a Graph account asked about SMTP, and a stray parameter.
    #[test]
    fn a_probe_that_cannot_run_is_refused_before_it_dials() {
        let refused = |params: Value| {
            ProbeParams::from_params(&params)
                .expect_err("refused")
                .to_string()
        };
        assert!(refused(json!({"account": password_account(), "check": "pop3"})).contains("check"));
        assert!(
            refused(json!({"account": password_account(), "check": "smtp"})).contains("password")
        );
        let graph = json!({
            "name": "ms", "default_from": "me@example.com", "auth_method": "graph",
            "oauth2": {"client_id": "c", "tenant_id": "common"},
        });
        assert!(refused(json!({"account": graph, "check": "smtp"})).contains("graph"));
        assert!(
            refused(json!({"account": password_account(), "check": "imap", "pw": "x"}))
                .contains("pw")
        );
    }
}
