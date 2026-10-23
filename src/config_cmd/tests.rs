//! The candidates the setup wizards hand the daemon, read back through the
//! loader the daemon reads them with
//! (`daemon::methods::config::candidate_account`), so what a wizard describes
//! is what `config.toml` ends up holding.

use serde_json::Value;

use super::init::{graph_candidate, imap_candidate, parse_override, ImapAnswers, Mailboxes, Probe};
use crate::config::{AccountConfig, AuthMethod, FolderPolicy};
use crate::daemon::methods::config::candidate_account;

fn loaded(candidate: &Value) -> AccountConfig {
    candidate_account(candidate).expect("the candidate loads")
}

fn mapped() -> Mailboxes {
    Mailboxes {
        inbox: "INBOX".into(),
        archive: "Archive".into(),
        sent: "Sent".into(),
        trash: Some("Papierkorb".into()),
        overrides: vec![("Labels/*".into(), FolderPolicy::Ignore)],
        ..Default::default()
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
    assert!(
        account.mailboxes.extra.is_none(),
        "the daemon syncs every listed folder, so the wizard pins none (#0141)"
    );
    assert_eq!(
        account.mailboxes.trash.as_ref().unwrap().server,
        "Papierkorb"
    );
    assert!(account.mailboxes.junk.is_none());
    let rules: Vec<(&str, FolderPolicy)> = account
        .mailboxes
        .overrides
        .iter()
        .map(|rule| (rule.pattern.as_str(), rule.policy))
        .collect();
    assert_eq!(rules, [("Labels/*", FolderPolicy::Ignore)]);
}

/// An override line is a pattern, spaces allowed, then its policy.
#[test]
fn an_override_line_reads_its_pattern_and_policy() {
    assert_eq!(
        parse_override("Sent Items/Old  status-only"),
        Ok(("Sent Items/Old".to_string(), FolderPolicy::StatusOnly))
    );
    assert_eq!(
        parse_override("Labels/* ignore"),
        Ok(("Labels/*".to_string(), FolderPolicy::Ignore))
    );
    assert!(parse_override("Labels/*").is_err());
    assert!(parse_override("Labels/* sometimes").is_err());
    assert!(parse_override(" sync").is_err());
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

/// The provider menu offers Microsoft 365 through Graph only: no line
/// offers Microsoft OAuth2 over IMAP and SMTP, every Microsoft line picks the
/// Graph flow, on-premises Exchange is the standard password account, and
/// the digit that used to pick the retired mode falls back to the default.
#[test]
fn the_provider_menu_offers_microsoft_365_through_graph_only() {
    use super::init::{preset_for, Preset, PROVIDERS};
    for (digit, line, preset) in PROVIDERS {
        assert!(!line.contains("OAuth2"), "{line}");
        if line.contains("Microsoft") {
            assert_eq!(preset, Preset::Graph, "{line}");
        }
        assert_eq!(preset_for(digit), preset);
    }
    assert_eq!(preset_for("3"), Preset::Graph);
    assert_eq!(preset_for("4"), Preset::Standard);
    let standard = PROVIDERS[0].1;
    assert!(standard.contains("on-premises Exchange"), "{standard}");
    assert!(imap_candidate(&answers()).get("auth_method").is_none());
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
        junk: Some("Say \"hi\"".into()),
        overrides: vec![(odd.into(), FolderPolicy::Sync)],
        ..Default::default()
    };
    let account = loaded(&imap_candidate(&ImapAnswers {
        default_from: from.into(),
        smtp_username: from.into(),
        mailboxes: mailboxes.clone(),
        ..answers()
    }));
    assert_eq!(account.default_from, from);
    assert_eq!(account.smtp.username, from);
    assert_eq!(account.mailboxes.inbox.as_ref().unwrap().server, odd);

    let graph = loaded(&graph_candidate(
        "g",
        from,
        "client\"id",
        "tenant\\id",
        &mailboxes,
    ));
    assert_eq!(graph.default_from, from);
    let oauth2 = graph.oauth2.as_ref().expect("oauth2");
    assert_eq!(
        (oauth2.client_id.as_str(), oauth2.tenant_id.as_str()),
        ("client\"id", "tenant\\id")
    );
    assert_eq!(graph.mailboxes.junk.as_ref().unwrap().server, "Say \"hi\"");
    assert_eq!(graph.mailboxes.overrides[0].pattern, odd);
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

/// A server that lists only INBOX still gets every role: the inbox is picked
/// from the list, with a name to type besides, and the archive and the sent
/// folder, with nothing left to pick, are typed, Enter taking the usual name.
#[test]
fn a_server_listing_only_inbox_lets_the_other_roles_be_typed() {
    use super::init::{mailbox_prompt, picked, MailboxPrompt, TYPE_ANOTHER};
    let mut available = vec!["INBOX".to_string()];
    let MailboxPrompt::Pick { items, default } = mailbox_prompt(&available, ("INBOX", &["INBOX"]))
    else {
        panic!("a listed inbox is picked");
    };
    assert_eq!(items, ["INBOX", TYPE_ANOTHER]);
    assert_eq!(picked(&items, default).as_deref(), Some("INBOX"));
    assert_eq!(picked(&items, 1), None, "the last item asks for a name");
    available.retain(|m| m != "INBOX");
    assert_eq!(
        mailbox_prompt(&available, ("Archive", &["Archive", "All Mail"])),
        MailboxPrompt::Type {
            default: "Archive".into()
        }
    );
    assert_eq!(
        mailbox_prompt(&available, ("Sent", &["Sent"])),
        MailboxPrompt::Type {
            default: "Sent".into()
        }
    );
}

/// One mailbox plays one of the inbox, archive and sent roles, and none of
/// them besides drafts, trash or junk, which may share one, as the desktop
/// wizard rules: a typed name a role holds is refused with the desktop's
/// words and asked again instead of ending the wizard.
#[test]
fn a_typed_mailbox_another_role_holds_is_asked_again() {
    use super::init::{mailbox_twice, untaken};
    let taken = vec![
        ("Inbox", "INBOX".to_string()),
        ("Archive", "Archive".to_string()),
    ];
    assert_eq!(
        mailbox_twice("mailbox", &taken, "INBOX").as_deref(),
        Some("INBOX is already the inbox mailbox")
    );
    assert_eq!(
        mailbox_twice("folder", &taken, " Archive ").as_deref(),
        Some("Archive is already the archive folder")
    );
    assert_eq!(mailbox_twice("mailbox", &taken, "Sent"), None);
    assert_eq!(
        mailbox_twice("mailbox", &taken, ""),
        None,
        "an empty answer leaves a marked role to the server"
    );

    let mut answers = vec!["INBOX", "Archive", "Sent"].into_iter();
    let mut asked = 0;
    let sent = untaken("mailbox", &taken, || {
        asked += 1;
        Ok(answers.next().expect("asked once too often").to_string())
    })
    .unwrap();
    assert_eq!((sent.as_str(), asked), ("Sent", 3));

    // Drafts, trash and junk never enter `taken`, so each of them may name
    // the folder another of them named, but none the sent folder.
    let all = vec![
        ("Inbox", "INBOX".to_string()),
        ("Archive", "Archive".to_string()),
        ("Sent", "Sent".to_string()),
    ];
    let mut answers = vec!["Sent", "Bin"].into_iter();
    let junk = untaken("mailbox", &all, || Ok(answers.next().unwrap().into())).unwrap();
    assert_eq!(junk, "Bin", "the trash folder named Bin before");
}

/// A daemon double whose `config.set_password` refuses one kind.
struct RefusingSetPassword {
    refuse: &'static str,
    stored: Vec<String>,
}

impl super::WizardDaemon for RefusingSetPassword {
    async fn probe(&mut self, _: &Value, _: &str, _: Option<&str>) -> anyhow::Result<Probe> {
        unreachable!("no probe in this test")
    }
    async fn sign_in(&mut self, _: &Value) -> anyhow::Result<()> {
        unreachable!("no sign-in in this test")
    }
    async fn init(&mut self, _: &Value, _: bool) -> anyhow::Result<std::path::PathBuf> {
        unreachable!("no write in this test")
    }
    async fn add_account(&mut self, _: &Value) -> anyhow::Result<()> {
        unreachable!("no write in this test")
    }
    async fn set_password(&mut self, _: &str, kind: &str, _: &str) -> anyhow::Result<()> {
        if kind == self.refuse {
            anyhow::bail!("the backend is locked");
        }
        self.stored.push(kind.to_string());
        Ok(())
    }
}

/// A password the daemon fails to store after the file is written is not
/// silent: the wizard tries the others, then fails naming the account, the
/// password and the command that sets it.
#[tokio::test]
async fn a_password_that_was_not_stored_is_named_with_its_command() {
    let mut daemon = RefusingSetPassword {
        refuse: "smtp",
        stored: Vec::new(),
    };
    let passwords = [("smtp", "a".to_string()), ("imap", "b".to_string())];
    let error = super::init::store_passwords(&mut daemon, "work", &passwords)
        .await
        .expect_err("a refused password fails the wizard");
    assert_eq!(
        daemon.stored,
        ["imap"],
        "the other password is still stored"
    );
    let message = error.to_string();
    assert!(message.contains("Account 'work'"), "{message}");
    assert!(message.contains("SMTP password is not stored"), "{message}");
    assert!(
        message.contains("mp config set-password smtp --account work"),
        "{message}"
    );
    assert!(!message.contains("set-password imap"), "{message}");

    let both = super::init::missing_passwords_message("work", &["smtp", "imap"]);
    assert!(
        both.contains("SMTP and IMAP passwords are not stored"),
        "{both}"
    );
}
