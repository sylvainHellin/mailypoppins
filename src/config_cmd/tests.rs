use super::init::{build_add_account_toml, build_init_toml};

#[test]
fn test_build_init_toml_basic() {
    let toml = build_init_toml(
        "main", "user@example.com",
        "smtp.example.com", 465, "user@example.com", false,
        "", 993, "",
        "INBOX",
        "Archive",
        "Sent",
        &[],
        None,
    );

    assert!(toml.contains("[email]"));
    assert!(toml.contains("font_family = \"Helvetica, Arial, sans-serif\""));
    assert!(toml.contains("[[accounts]]"));
    assert!(toml.contains("name = \"main\""));
    assert!(toml.contains("default_from = \"user@example.com\""));
    assert!(toml.contains("[accounts.smtp]"));
    assert!(toml.contains("host = \"smtp.example.com\""));
    assert!(toml.contains("port = 465"));
    assert!(toml.contains("[accounts.imap]"));
    assert!(toml.contains("port = 993"));
    // Removed: [accounts.directories], local = "..."
    assert!(!toml.contains("[accounts.directories]"));
    assert!(!toml.contains("local ="));
    assert!(toml.contains("[accounts.mailboxes.inbox]"));
    assert!(toml.contains("server = \"INBOX\""));
    assert!(toml.contains("[accounts.mailboxes.archive]"));
    assert!(toml.contains("[accounts.mailboxes.sent]"));
    // No accept_invalid_certs when false
    assert!(!toml.contains("accept_invalid_certs"));
}

#[test]
fn test_build_init_toml_proton_bridge() {
    let toml = build_init_toml(
        "proton", "user@proton.me",
        "127.0.0.1", 1025, "user@proton.me", true,
        "127.0.0.1", 1143, "user@proton.me",
        "INBOX",
        "All Mail",
        "Sent",
        &[],
        None,
    );

    assert!(toml.contains("accept_invalid_certs = true"));
    assert!(toml.contains("host = \"127.0.0.1\""));
    assert!(toml.contains("port = 1025"));
    assert!(toml.contains("port = 1143"));
    let imap_section = toml.find("[accounts.imap]").unwrap();
    let after_imap = &toml[imap_section..];
    assert!(after_imap.contains("host = \"127.0.0.1\""));
}

#[test]
fn test_build_init_toml_imap_host_omitted_when_empty() {
    let toml = build_init_toml(
        "main", "user@example.com",
        "smtp.example.com", 465, "user@example.com", false,
        "", 993, "",
        "INBOX",
        "Archive",
        "Sent",
        &[],
        None,
    );

    let imap_pos = toml.find("[accounts.imap]").unwrap();
    let imap_end = toml[imap_pos..].find("\n[").map(|p| imap_pos + p).unwrap_or(toml.len());
    let imap_section = &toml[imap_pos..imap_end];
    assert!(!imap_section.contains("host ="));
}

#[test]
fn test_build_init_toml_with_extra_mailboxes() {
    let extras = vec!["Junk".to_string(), "Trash".to_string()];
    let toml = build_init_toml(
        "main", "user@example.com",
        "smtp.example.com", 465, "user@example.com", false,
        "", 993, "",
        "INBOX",
        "Archive",
        "Sent",
        &extras,
        None,
    );

    assert!(toml.contains("[[accounts.mailboxes.extra]]"));
    assert!(toml.contains("server = \"Junk\""));
    assert!(toml.contains("server = \"Trash\""));
    // Local paths are now derived from the data dir, not stored in config.
    assert!(!toml.contains("local ="));
}

#[test]
fn test_build_init_toml_parseable() {
    let toml_str = build_init_toml(
        "test", "test@example.com",
        "smtp.example.com", 465, "test@example.com", false,
        "", 993, "",
        "INBOX",
        "Archive",
        "Sent",
        &[],
        None,
    );

    // Verify the generated TOML is actually parseable as a valid GlobalConfig
    let config: crate::config::GlobalConfig = toml::from_str(&toml_str)
        .expect("Generated TOML should be parseable as GlobalConfig");
    assert_eq!(config.accounts.len(), 1);
    assert_eq!(config.accounts[0].name, "test");
    assert_eq!(config.accounts[0].default_from, "test@example.com");
    assert_eq!(config.accounts[0].smtp.host, "smtp.example.com");
}

#[test]
fn test_build_add_account_toml_basic() {
    let block = build_add_account_toml(
        "work", "work@corp.com",
        "smtp.corp.com", 587, "work@corp.com", false,
        "imap.corp.com", 993, "work@corp.com",
        "INBOX",
        "Archive",
        "Sent",
        &[],
        None,
    );

    assert!(block.starts_with("\n[[accounts]]"));
    assert!(block.contains("name = \"work\""));
    assert!(block.contains("default_from = \"work@corp.com\""));
    assert!(block.contains("host = \"smtp.corp.com\""));
    assert!(block.contains("host = \"imap.corp.com\""));
    assert!(!block.contains("[accounts.directories]"));
    assert!(!block.contains("local ="));
}

#[test]
fn test_build_add_account_toml_appended_is_valid() {
    let base = build_init_toml(
        "main", "main@example.com",
        "smtp.example.com", 465, "main@example.com", false,
        "", 993, "",
        "INBOX",
        "Archive",
        "Sent",
        &[],
        None,
    );

    let addition = build_add_account_toml(
        "work", "work@corp.com",
        "smtp.corp.com", 587, "work@corp.com", false,
        "imap.corp.com", 993, "work@corp.com",
        "INBOX",
        "Archive",
        "Sent",
        &[],
        None,
    );

    let combined = format!("{}{}", base, addition);
    let config: crate::config::GlobalConfig = toml::from_str(&combined)
        .expect("Combined TOML should be parseable");
    assert_eq!(config.accounts.len(), 2);
    assert_eq!(config.accounts[0].name, "main");
    assert_eq!(config.accounts[1].name, "work");
}

#[test]
fn test_build_init_toml_oauth2_exchange() {
    let toml_str = build_init_toml(
        "work", "user@example.com",
        "smtp.office365.com", 587, "user@example.com", false,
        "outlook.office365.com", 993, "",
        "INBOX",
        "Archive",
        "Sent Items",
        &[],
        Some(("test-client-id", "test-tenant-id")),
    );

    assert!(toml_str.contains("auth_method = \"oauth2\""));
    assert!(toml_str.contains("[accounts.oauth2]"));
    assert!(toml_str.contains("client_id = \"test-client-id\""));
    assert!(toml_str.contains("tenant_id = \"test-tenant-id\""));
    assert!(toml_str.contains("host = \"smtp.office365.com\""));
    assert!(toml_str.contains("host = \"outlook.office365.com\""));
    assert!(toml_str.contains("port = 587"));

    let config: crate::config::GlobalConfig = toml::from_str(&toml_str)
        .expect("OAuth2 TOML should be parseable");
    assert_eq!(config.accounts[0].name, "work");
    assert_eq!(config.accounts[0].auth_method, crate::config::AuthMethod::OAuth2);
    let oauth2 = config.accounts[0].oauth2.as_ref().unwrap();
    assert_eq!(oauth2.client_id, "test-client-id");
    assert_eq!(oauth2.tenant_id, "test-tenant-id");
}

#[test]
fn test_build_init_toml_password_no_oauth2_section() {
    let toml_str = build_init_toml(
        "test", "test@example.com",
        "smtp.example.com", 465, "test@example.com", false,
        "", 993, "",
        "INBOX",
        "Archive",
        "Sent",
        &[],
        None,
    );

    assert!(!toml_str.contains("auth_method"));
    assert!(!toml_str.contains("[accounts.oauth2]"));

    let config: crate::config::GlobalConfig = toml::from_str(&toml_str)
        .expect("Password TOML should be parseable");
    assert_eq!(config.accounts[0].auth_method, crate::config::AuthMethod::Password);
    assert!(config.accounts[0].oauth2.is_none());
}

/// The wizard's completion block promises a directory that exists.
///
/// The file-era block printed an inbox, a sent and an archive directory that
/// nothing created; this pins the replacement, whose first line is the account
/// directory the store and its blobs are written into.
#[test]
fn account_data_paths_creates_the_directory_it_prints() {
    let _tmp = crate::config::test_env::TestDataDir::new();

    let acct_dir = crate::config::account_dir("wizard");
    assert!(!acct_dir.exists());
    super::init::print_account_data_paths("wizard");

    assert!(acct_dir.is_dir(), "{} was not created", acct_dir.display());
    assert_eq!(crate::config::store_path("wizard").parent(), Some(acct_dir.as_path()));
    assert_eq!(crate::config::blobs_dir("wizard").parent(), Some(acct_dir.as_path()));
    assert_eq!(crate::config::drafts_dir("wizard").parent(), Some(acct_dir.as_path()));
}

/// A display name with quotes and a comma, and a server mailbox name with a
/// backslash, are wizard answers like any other: the generated TOML parses and
/// hands both back unchanged, on the init path and on the add-account path.
#[test]
fn wizard_answers_needing_toml_escapes_round_trip() {
    let from = "\"Doe, Jane\" <j@x>";
    let odd_box = "Back\\slash";
    let extra = vec![odd_box.to_string(), "Say \"hi\"".to_string()];
    let base = build_init_toml(
        "main", from,
        "smtp.example.com", 465, from, false,
        "", 993, "",
        odd_box,
        "Archive",
        "Sent",
        &extra,
        None,
    );
    let addition = build_add_account_toml(
        "work", from,
        "smtp.corp.com", 587, "work@corp.com", false,
        "imap.corp.com", 993, "work@corp.com",
        "INBOX",
        odd_box,
        "Sent",
        &extra,
        Some(("client\"id", "tenant\\id")),
    );
    let combined = format!("{base}{addition}");
    super::init::ensure_config_parses(&combined).expect("the escaped config parses");
    let config: crate::config::GlobalConfig =
        toml::from_str(&combined).expect("the escaped config parses");

    assert_eq!(config.accounts.len(), 2);
    for account in &config.accounts {
        assert_eq!(account.default_from, from);
        let extras: Vec<&str> = account
            .mailboxes
            .extra
            .as_ref()
            .expect("extra mailboxes")
            .iter()
            .map(|m| m.server.as_str())
            .collect();
        assert_eq!(extras, vec![odd_box, "Say \"hi\""]);
    }
    assert_eq!(config.accounts[0].smtp.username, from);
    assert_eq!(config.accounts[0].mailboxes.inbox.as_ref().unwrap().server, odd_box);
    assert_eq!(config.accounts[1].mailboxes.archive.as_ref().unwrap().server, odd_box);
    let oauth2 = config.accounts[1].oauth2.as_ref().expect("oauth2 section");
    assert_eq!((oauth2.client_id.as_str(), oauth2.tenant_id.as_str()), ("client\"id", "tenant\\id"));

    let graph = super::init::build_graph_account_toml(
        "g", from, "client", "tenant", odd_box, "Archive", "Sent", &extra,
    );
    let config: crate::config::GlobalConfig = toml::from_str(&graph).expect("graph block parses");
    assert_eq!(config.accounts[0].default_from, from);
    assert_eq!(config.accounts[0].mailboxes.inbox.as_ref().unwrap().server, odd_box);
}

/// The backstop before every write: text the loader would reject is refused
/// with an error that says nothing was written.
#[test]
fn a_config_that_does_not_parse_is_refused() {
    let err = super::init::ensure_config_parses("[[accounts]]\nname = \"a\"b\"\n").unwrap_err();
    assert!(format!("{err:#}").contains("not written"), "{err:#}");
}
