//! The candidates the setup wizards hand the daemon, read back through the
//! loader the daemon reads them with
//! (`daemon::methods::config::candidate_account`), so what a wizard describes
//! is what `config.toml` ends up holding.

use serde_json::Value;

use super::init::{graph_candidate, imap_candidate, ImapAnswers, Mailboxes, Probe};
use crate::config::{AccountConfig, AuthMethod};
use crate::daemon::methods::config::candidate_account;

fn loaded(candidate: &Value) -> AccountConfig {
    candidate_account(candidate).expect("the candidate loads")
}

fn mapped() -> Mailboxes {
    Mailboxes {
        inbox: "INBOX".into(),
        archive: "Archive".into(),
        sent: "Sent".into(),
        extra: vec!["Projects".into()],
    }
}

fn answers() -> ImapAnswers {
    ImapAnswers {
        name: "main".into(),
        default_from: "me@example.com".into(),
        smtp_host: "smtp.example.com".into(),
        smtp_port: 465,
        smtp_username: "me@example.com".into(),
        imap_host: String::new(),
        imap_port: 993,
        imap_username: String::new(),
        accept_invalid_certs: false,
        oauth2: None,
        mailboxes: mapped(),
    }
}

/// A password account: SMTP as typed, an empty IMAP host and username left
/// out so the loader falls back to the SMTP ones, and the four mappings.
#[test]
fn a_password_candidate_loads_with_its_fallbacks_and_mailboxes() {
    let account = loaded(&imap_candidate(&answers()));
    assert_eq!(account.name, "main");
    assert_eq!(account.auth_method, AuthMethod::Password);
    assert_eq!(account.smtp.host, "smtp.example.com");
    assert_eq!(account.smtp.port, 465);
    assert_eq!(
        account.imap.host, "",
        "an empty host is left to the fallback"
    );
    assert_eq!(account.imap.username, "");
    assert_eq!(account.imap.port, 993);
    assert!(account.oauth2.is_none());
    assert_eq!(account.mailboxes.inbox.as_ref().unwrap().server, "INBOX");
    assert_eq!(
        account.mailboxes.archive.as_ref().unwrap().server,
        "Archive"
    );
    assert_eq!(account.mailboxes.sent.as_ref().unwrap().server, "Sent");
    let extra: Vec<&str> = account
        .mailboxes
        .extra
        .as_ref()
        .expect("extra")
        .iter()
        .map(|m| m.server.as_str())
        .collect();
    assert_eq!(extra, ["Projects"]);
}

/// Proton Bridge: both servers accept the bridge's self-signed certificate.
#[test]
fn a_proton_candidate_accepts_invalid_certs_on_both_servers() {
    let account = loaded(&imap_candidate(&ImapAnswers {
        smtp_host: "127.0.0.1".into(),
        smtp_port: 1025,
        imap_host: "127.0.0.1".into(),
        imap_port: 1143,
        accept_invalid_certs: true,
        ..answers()
    }));
    assert!(account.smtp.accept_invalid_certs);
    assert!(account.imap.accept_invalid_certs);
    assert_eq!(account.imap.host, "127.0.0.1");
    assert_eq!(account.imap.port, 1143);
}

/// Exchange over OAuth2: `auth_method = "oauth2"` and the app registration.
#[test]
fn an_exchange_candidate_carries_its_app_registration() {
    let account = loaded(&imap_candidate(&ImapAnswers {
        oauth2: Some(("client".into(), "tenant".into())),
        ..answers()
    }));
    assert_eq!(account.auth_method, AuthMethod::OAuth2);
    let oauth2 = account.oauth2.expect("oauth2");
    assert_eq!(
        (oauth2.client_id.as_str(), oauth2.tenant_id.as_str()),
        ("client", "tenant")
    );
}

/// A probe made before the mailboxes are chosen carries no mapping, and still
/// loads.
#[test]
fn a_candidate_without_mailboxes_loads_for_a_probe() {
    let candidate = imap_candidate(&ImapAnswers {
        mailboxes: Mailboxes::default(),
        ..answers()
    });
    assert!(candidate.get("mailboxes").is_none(), "{candidate}");
    assert!(loaded(&candidate).mailboxes.inbox.is_none());
}

/// A display name with quotes and a comma, and server mailbox names with a
/// backslash or quotes, are wizard answers like any other: the block the
/// daemon writes from them loads and hands them back unchanged.
#[test]
fn wizard_answers_needing_toml_escapes_round_trip() {
    let from = "\"Doe, Jane\" <j@x>";
    let odd = "Back\\slash";
    let mailboxes = Mailboxes {
        inbox: odd.into(),
        archive: "Archive".into(),
        sent: "Sent".into(),
        extra: vec![odd.into(), "Say \"hi\"".into()],
    };
    let account = loaded(&imap_candidate(&ImapAnswers {
        default_from: from.into(),
        smtp_username: from.into(),
        oauth2: Some(("client\"id".into(), "tenant\\id".into())),
        mailboxes: mailboxes.clone(),
        ..answers()
    }));
    assert_eq!(account.default_from, from);
    assert_eq!(account.smtp.username, from);
    assert_eq!(account.mailboxes.inbox.as_ref().unwrap().server, odd);
    let oauth2 = account.oauth2.expect("oauth2");
    assert_eq!(
        (oauth2.client_id.as_str(), oauth2.tenant_id.as_str()),
        ("client\"id", "tenant\\id")
    );

    let graph = loaded(&graph_candidate("g", from, "client", "tenant", &mailboxes));
    assert_eq!(graph.default_from, from);
    let extra: Vec<&str> = graph
        .mailboxes
        .extra
        .as_ref()
        .expect("extra")
        .iter()
        .map(|m| m.server.as_str())
        .collect();
    assert_eq!(extra, [odd, "Say \"hi\""]);
}

/// PERSO-164: Enter at the Graph wizard's client ID and tenant prompts writes
/// the shared mailypoppins app and the `common` tenant, and the account loads
/// as a Graph account with them; a typed client ID and tenant are written as
/// they are, so an account with its own app registration keeps working.
#[test]
fn graph_wizard_defaults_to_the_mailypoppins_app_and_common() {
    for (client_in, tenant_in, client_out, tenant_out) in [
        (
            "",
            "",
            crate::oauth2::MAILYPOPPINS_CLIENT_ID,
            crate::oauth2::DEFAULT_TENANT,
        ),
        (
            "00000000-1111-2222-3333-444444444444",
            "contoso.onmicrosoft.com",
            "00000000-1111-2222-3333-444444444444",
            "contoso.onmicrosoft.com",
        ),
    ] {
        let client_id = crate::oauth2::client_id_or_default(client_in);
        let tenant_id = crate::oauth2::tenant_or_default(tenant_in);
        let account = loaded(&graph_candidate(
            "work",
            "me@example.com",
            &client_id,
            &tenant_id,
            &mapped(),
        ));
        assert_eq!(account.auth_method, AuthMethod::Graph);
        let graph = crate::config::GraphConfig::load(&account).expect("graph config");
        assert_eq!(graph.client_id, client_out);
        assert_eq!(graph.tenant_id, tenant_out);
    }
}

/// A `config.probe` answer reads into a [`Probe`], a failure with its reason
/// and a listing with its names.
#[test]
fn a_probe_answer_reads_its_verdict_reason_and_names() {
    let failed = Probe::from_answer(&serde_json::json!({"ok": false, "error": "refused"}));
    assert!(!failed.ok);
    assert_eq!(failed.error.as_deref(), Some("refused"));
    let listed =
        Probe::from_answer(&serde_json::json!({"ok": true, "mailboxes": ["INBOX", "Sent"]}));
    assert!(listed.ok);
    assert_eq!(listed.mailboxes, ["INBOX", "Sent"]);
}
