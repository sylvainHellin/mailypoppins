use super::init::{build_add_account_toml, build_init_toml};

#[test]
fn test_build_init_toml_basic() {
    let toml = build_init_toml(
        "main", "user@example.com",
        "smtp.example.com", 465, "user@example.com", false,
        "", 993, "",
        "~/notes/email", "drafts",
        "INBOX", "inbox",
        "Archive", "archive",
        "Sent", "sent",
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
    assert!(toml.contains("[accounts.directories]"));
    assert!(toml.contains("root = \"~/notes/email\""));
    assert!(toml.contains("drafts = \"drafts\""));
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
        "~/notes/email", "drafts",
        "INBOX", "inbox",
        "All Mail", "all-mail",
        "Sent", "sent",
        &[],
        None,
    );

    assert!(toml.contains("accept_invalid_certs = true"));
    assert!(toml.contains("host = \"127.0.0.1\""));
    assert!(toml.contains("port = 1025"));
    assert!(toml.contains("port = 1143"));
    // IMAP host should be included when non-empty
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
        "~/notes/email", "drafts",
        "INBOX", "inbox",
        "Archive", "archive",
        "Sent", "sent",
        &[],
        None,
    );

    // IMAP section should NOT have a host line (falls back to SMTP)
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
        "~/notes/email", "drafts",
        "INBOX", "inbox",
        "Archive", "archive",
        "Sent", "sent",
        &extras,
        None,
    );

    assert!(toml.contains("[[accounts.mailboxes.extra]]"));
    assert!(toml.contains("server = \"Junk\""));
    assert!(toml.contains("server = \"Trash\""));
    assert!(toml.contains("local = \"junk\""));
    assert!(toml.contains("local = \"trash\""));
}

#[test]
fn test_build_init_toml_parseable() {
    let toml_str = build_init_toml(
        "test", "test@example.com",
        "smtp.example.com", 465, "test@example.com", false,
        "", 993, "",
        "~/mail", "drafts",
        "INBOX", "inbox",
        "Archive", "archive",
        "Sent", "sent",
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
        "~/mail/work", "drafts",
        "INBOX", "inbox",
        "Archive", "archive",
        "Sent", "sent",
        &[],
    );

    assert!(block.starts_with("\n[[accounts]]"));
    assert!(block.contains("name = \"work\""));
    assert!(block.contains("default_from = \"work@corp.com\""));
    assert!(block.contains("host = \"smtp.corp.com\""));
    assert!(block.contains("host = \"imap.corp.com\""));
}

#[test]
fn test_build_add_account_toml_appended_is_valid() {
    // Build initial config
    let base = build_init_toml(
        "main", "main@example.com",
        "smtp.example.com", 465, "main@example.com", false,
        "", 993, "",
        "~/mail", "drafts",
        "INBOX", "inbox",
        "Archive", "archive",
        "Sent", "sent",
        &[],
        None,
    );

    // Build second account
    let addition = build_add_account_toml(
        "work", "work@corp.com",
        "smtp.corp.com", 587, "work@corp.com", false,
        "imap.corp.com", 993, "work@corp.com",
        "~/mail/work", "drafts",
        "INBOX", "inbox",
        "Archive", "archive",
        "Sent", "sent",
        &[],
    );

    let combined = format!("{}{}", base, addition);
    let config: crate::config::GlobalConfig = toml::from_str(&combined)
        .expect("Combined TOML should be parseable");
    assert_eq!(config.accounts.len(), 2);
    assert_eq!(config.accounts[0].name, "main");
    assert_eq!(config.accounts[1].name, "work");
}
