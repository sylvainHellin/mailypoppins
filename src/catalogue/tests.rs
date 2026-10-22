//! The catalogue's rules, against the two live accounts' listings as the
//! 2026-10-09 code map and #0143's probe recorded them.

use super::*;
use crate::config::{MailboxMapping, MailboxOverride, MailboxesConfig};

fn mapping(server: &str) -> Option<MailboxMapping> {
    Some(MailboxMapping {
        server: server.to_string(),
    })
}

fn listed(name: &str, attributes: &[&str]) -> ListedFolder {
    ListedFolder::named(name).with_attributes(attributes)
}

/// TUM's 24 folders, with the marks Exchange sends in a plain `LIST`.
fn tum_listing() -> Vec<ListedFolder> {
    [
        ("Archive", &[][..]),
        ("Archive1", &[]),
        ("Calendar", &[]),
        ("Calendar/Birthdays", &[]),
        ("Contacts", &[]),
        ("Conversation History", &[]),
        ("Deleted Items", &["\\Trash"]),
        ("Drafts", &["\\Drafts"]),
        ("INBOX", &["\\Marked"]),
        ("INBOX/reimbursements", &[]),
        ("Journal", &[]),
        ("Junk Email", &["\\Junk"]),
        ("Notes", &[]),
        ("Outbox", &[]),
        ("Read", &[]),
        ("RSS Feeds", &[]),
        ("Scheduled", &[]),
        ("Sent Items", &["\\Sent"]),
        ("Spesen", &[]),
        ("Sync Issues", &[]),
        ("Sync Issues/Conflicts", &[]),
        ("Sync Issues/Local Failures", &[]),
        ("Sync Issues/Server Failures", &[]),
        ("Tasks", &[]),
    ]
    .iter()
    .map(|(name, attributes)| listed(name, attributes))
    .collect()
}

fn tum_account() -> AccountConfig {
    AccountConfig {
        name: "tum".to_string(),
        mailboxes: MailboxesConfig {
            inbox: mapping("INBOX"),
            archive: mapping("Archive"),
            sent: mapping("Sent Items"),
            extra: Some(vec![MailboxMapping {
                server: "Spesen".to_string(),
            }]),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Proton Bridge's 17 folders, in the order Bridge lists them.
fn proton_listing() -> Vec<ListedFolder> {
    [
        ("Folders", &["\\Noselect"][..]),
        ("Spam", &["\\Junk"]),
        ("Labels/taxes", &[]),
        ("INBOX", &[]),
        ("Trash", &["\\Trash"]),
        ("Archive", &["\\Archive"]),
        ("Labels/Haus", &[]),
        ("Labels", &["\\Noselect"]),
        ("Labels/Receipt", &[]),
        ("Labels/AI", &[]),
        ("Labels/Gmail", &[]),
        ("All Mail", &["\\All"]),
        ("Labels/vacances", &[]),
        ("Starred", &["\\Flagged"]),
        ("Sent", &["\\Sent"]),
        ("Drafts", &["\\Drafts"]),
        ("Labels/Puppy", &[]),
    ]
    .iter()
    .map(|(name, attributes)| listed(name, attributes))
    .collect()
}

fn proton_account() -> AccountConfig {
    AccountConfig {
        name: "proton".to_string(),
        mailboxes: MailboxesConfig {
            inbox: mapping("INBOX"),
            archive: mapping("Archive"),
            sent: mapping("Sent"),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn targets(catalogue: &Catalogue) -> Vec<(String, String, Option<usize>)> {
    catalogue
        .targets()
        .into_iter()
        .map(|t| (t.role.as_str().to_string(), t.server_name, t.window))
        .collect()
}

fn policy_of(catalogue: &Catalogue, server: &str) -> FolderPolicy {
    catalogue
        .folders
        .iter()
        .find(|f| f.server == server)
        .unwrap_or_else(|| panic!("{server} is in the catalogue"))
        .policy
}

/// Acceptance: a quick tick on TUM selects only the `sync` folders, plus the
/// tail of Deleted Items and Junk Email; the five user folders the ticket
/// names are among them and nothing non-mail is.
#[test]
fn tum_syncs_its_role_and_user_folders_and_tails_trash_and_junk() {
    let catalogue = Catalogue::resolve(&tum_account(), &tum_listing());
    let w = Some(STATUS_ONLY_WINDOW);
    assert_eq!(
        targets(&catalogue),
        vec![
            ("inbox".into(), "INBOX".into(), None),
            ("archive".into(), "Archive".into(), None),
            ("sent".into(), "Sent Items".into(), None),
            ("Spesen".into(), "Spesen".into(), None),
            ("Deleted Items".into(), "Deleted Items".into(), w),
            ("Junk Email".into(), "Junk Email".into(), w),
            ("Archive1".into(), "Archive1".into(), None),
            (
                "INBOX/reimbursements".into(),
                "INBOX/reimbursements".into(),
                None
            ),
            ("Read".into(), "Read".into(), None),
            ("Scheduled".into(), "Scheduled".into(), None),
        ]
    );
    for ignored in [
        "Calendar",
        "Calendar/Birthdays",
        "Contacts",
        "Conversation History",
        "Journal",
        "Notes",
        "Outbox",
        "RSS Feeds",
        "Sync Issues",
        "Sync Issues/Conflicts",
        "Tasks",
        "Drafts",
    ] {
        assert_eq!(
            policy_of(&catalogue, ignored),
            FolderPolicy::Ignore,
            "{ignored}"
        );
    }
    assert_eq!(
        policy_of(&catalogue, "Junk Email"),
        FolderPolicy::StatusOnly
    );
    assert_eq!(catalogue.role(FolderRole::Drafts).unwrap().server, "Drafts");
    assert_eq!(catalogue.role(FolderRole::Trash).unwrap().label, "Trash");

    let sidebar: Vec<&str> = catalogue
        .sidebar_extras()
        .map(|f| f.label.as_str())
        .collect();
    assert_eq!(
        sidebar,
        [
            "Trash",
            "Junk",
            "Spesen",
            "Archive1",
            "INBOX/reimbursements",
            "Read",
            "Scheduled"
        ]
    );
}

/// Acceptance: no Proton message is stored twice because of a label or All
/// Mail. Neither is ever a target; the labels keep their counts in the
/// sidebar, All Mail and Starred are not listed at all.
#[test]
fn proton_never_downloads_a_label_all_mail_or_starred() {
    let catalogue = Catalogue::resolve(&proton_account(), &proton_listing());
    let w = Some(STATUS_ONLY_WINDOW);
    assert_eq!(
        targets(&catalogue),
        vec![
            ("inbox".into(), "INBOX".into(), None),
            ("archive".into(), "Archive".into(), None),
            ("sent".into(), "Sent".into(), None),
            ("Trash".into(), "Trash".into(), w),
            ("Spam".into(), "Spam".into(), w),
        ]
    );
    for label in ["Labels/AI", "Labels/taxes", "Labels/Puppy"] {
        assert_eq!(
            policy_of(&catalogue, label),
            FolderPolicy::StatusOnly,
            "{label}"
        );
    }
    for ignored in ["All Mail", "Starred", "Folders", "Labels", "Drafts"] {
        assert_eq!(
            policy_of(&catalogue, ignored),
            FolderPolicy::Ignore,
            "{ignored}"
        );
    }
    let sidebar: Vec<&str> = catalogue
        .sidebar_extras()
        .map(|f| f.label.as_str())
        .collect();
    assert_eq!(
        sidebar,
        [
            "Trash",
            "Junk",
            "Labels/AI",
            "Labels/Gmail",
            "Labels/Haus",
            "Labels/Puppy",
            "Labels/Receipt",
            "Labels/taxes",
            "Labels/vacances"
        ]
    );
}

/// Acceptance: existing configs keep working unchanged. With nothing listed
/// the targets are exactly `all_configured_mailboxes`, and a listing does not
/// move a pinned extra off the key its rows carry, even one the server marks
/// with a keyed role.
#[test]
fn an_existing_config_keeps_its_targets_and_its_keys() {
    let account = tum_account();
    let before: Vec<(String, String)> = crate::config::all_configured_mailboxes(&account)
        .into_iter()
        .map(|(role, m)| (role.as_str().to_string(), m.server.clone()))
        .collect();
    let after: Vec<(String, String)> = Catalogue::resolve(&account, &[])
        .targets()
        .into_iter()
        .map(|t| (t.role.as_str().to_string(), t.server_name))
        .collect();
    assert_eq!(before, after);

    let pinned_sent = AccountConfig {
        mailboxes: MailboxesConfig {
            extra: Some(vec![MailboxMapping {
                server: "sent items".to_string(),
            }]),
            ..Default::default()
        },
        ..Default::default()
    };
    let catalogue = Catalogue::resolve(&pinned_sent, &[listed("Sent Items", &["\\Sent"])]);
    let folder = catalogue.find("Sent Items").unwrap();
    assert_eq!(
        folder.key, "sent items",
        "the extra's own spelling, as before"
    );
    assert_eq!(folder.policy, FolderPolicy::Sync);
    assert!(catalogue.role(FolderRole::Sent).is_none());
}

/// A role config does not name is found from the server's mark, keyed by the
/// role, so the sent copy and `in:sent` reach the folder without a config
/// entry.
#[test]
fn a_role_config_does_not_name_comes_from_the_listing() {
    let account = AccountConfig::default();
    let catalogue = Catalogue::resolve(
        &account,
        &[
            listed("INBOX", &[]),
            listed("Gesendete Elemente", &["\\Sent"]),
            listed("Projects", &[]),
        ],
    );
    let sent = catalogue.role(FolderRole::Sent).unwrap();
    assert_eq!((sent.key.as_str(), sent.label.as_str()), ("sent", "Sent"));
    assert_eq!(catalogue.server_for_key("sent"), "Gesendete Elemente");
    assert_eq!(catalogue.server_for_key("Projects"), "Projects");
    assert_eq!(
        catalogue.server_for_key("Never Listed"),
        "Never Listed",
        "a key is a server name"
    );
}

/// Dovecot's role folders sit under INBOX with a `.` delimiter and no marks.
#[test]
fn role_names_are_read_under_an_inbox_parent() {
    let folder = |name: &str| ListedFolder {
        name: name.to_string(),
        delimiter: Some(".".to_string()),
        ..Default::default()
    };
    let catalogue = Catalogue::resolve(
        &AccountConfig::default(),
        &[
            folder("INBOX"),
            folder("INBOX.Sent"),
            folder("INBOX.Trash"),
            folder("INBOX.Notes"),
        ],
    );
    assert_eq!(
        catalogue.role(FolderRole::Sent).unwrap().server,
        "INBOX.Sent"
    );
    assert_eq!(
        catalogue.role(FolderRole::Trash).unwrap().server,
        "INBOX.Trash"
    );
    assert_eq!(
        policy_of(&catalogue, "INBOX.Notes"),
        FolderPolicy::Sync,
        "Notes is only non-mail on Exchange"
    );
}

/// Acceptance: a server folder named `Drafts`, `Sent` or `Archive` never
/// shadows a role or the local drafts. A folder the server does not give the
/// role is ignored rather than keyed onto it, and a fetch from it is refused.
#[test]
fn a_folder_named_like_a_role_never_shadows_it() {
    let catalogue = Catalogue::resolve(
        &AccountConfig::default(),
        &[
            listed("INBOX", &[]),
            listed("Sent Messages", &["\\Sent"]),
            listed("Sent", &[]),
            listed("Old/Archive", &[]),
            listed("Archive", &[]),
            listed("Mail Archive", &["\\Archive"]),
            listed("drafts", &[]),
        ],
    );
    assert_eq!(
        catalogue.role(FolderRole::Sent).unwrap().server,
        "Sent Messages"
    );
    assert_eq!(
        catalogue.role(FolderRole::Archive).unwrap().server,
        "Mail Archive"
    );
    assert_eq!(policy_of(&catalogue, "Sent"), FolderPolicy::Ignore);
    assert_eq!(policy_of(&catalogue, "Archive"), FolderPolicy::Ignore);
    assert_eq!(policy_of(&catalogue, "Old/Archive"), FolderPolicy::Sync);
    // `drafts` is the name the server gives its drafts folder here, so it
    // holds the role, and stays out of the sidebar until #0143.
    assert_eq!(catalogue.role(FolderRole::Drafts).unwrap().server, "drafts");
    assert_eq!(policy_of(&catalogue, "drafts"), FolderPolicy::Ignore);
    assert!(catalogue
        .targets()
        .iter()
        .all(|t| t.server_name != "Sent" && t.server_name != "Archive"));

    assert!(catalogue.storage_for("Sent").is_err());
    assert!(catalogue.storage_for("drafts").is_err());
    assert!(
        catalogue.storage_for("Inbox").is_ok(),
        "the inbox is the role's own folder"
    );
}

/// `[[accounts.mailboxes.override]]`: the first matching glob decides, a pin
/// and an unselectable folder are beyond it.
#[test]
fn overrides_change_the_policy_except_of_pins_and_unselectable_folders() {
    let mut account = proton_account();
    account.mailboxes.overrides = vec![
        MailboxOverride {
            pattern: "labels/ai".to_string(),
            policy: FolderPolicy::Sync,
        },
        MailboxOverride {
            pattern: "Labels*".to_string(),
            policy: FolderPolicy::Ignore,
        },
        MailboxOverride {
            pattern: "Sent".to_string(),
            policy: FolderPolicy::Ignore,
        },
        MailboxOverride {
            pattern: "Spam".to_string(),
            policy: FolderPolicy::Sync,
        },
    ];
    let catalogue = Catalogue::resolve(&account, &proton_listing());
    assert_eq!(policy_of(&catalogue, "Labels/AI"), FolderPolicy::Sync);
    assert_eq!(policy_of(&catalogue, "Labels/Haus"), FolderPolicy::Ignore);
    assert_eq!(
        policy_of(&catalogue, "Labels"),
        FolderPolicy::Ignore,
        "still unselectable"
    );
    assert_eq!(policy_of(&catalogue, "Sent"), FolderPolicy::Sync, "a pin");
    assert_eq!(policy_of(&catalogue, "Spam"), FolderPolicy::Sync);
    let spam = catalogue
        .targets()
        .into_iter()
        .find(|t| t.server_name == "Spam")
        .unwrap();
    assert_eq!(spam.window, None, "a synced junk folder downloads in full");
}

/// A folder the server stopped listing keeps its rows but is neither synced
/// nor shown; a pin is synced whatever the listing says.
#[test]
fn a_missing_folder_is_kept_out_of_targets_and_the_sidebar() {
    let mut listing = tum_listing();
    for folder in &mut listing {
        if folder.name == "Read" || folder.name == "Spesen" {
            folder.missing = true;
        }
    }
    let catalogue = Catalogue::resolve(&tum_account(), &listing);
    let read = catalogue.find("Read").unwrap();
    assert!(read.missing && !read.listed());
    assert!(!catalogue.targets().iter().any(|t| t.server_name == "Read"));
    assert!(
        catalogue
            .targets()
            .iter()
            .any(|t| t.server_name == "Spesen"),
        "a pin"
    );
    assert!(!catalogue.sidebar_extras().any(|f| f.server == "Read"));
    assert_eq!(
        catalogue.storage_for("Read").unwrap().0,
        "Read",
        "its rows keep their key"
    );
}

/// The catalogue resolves the names a person or a client types.
#[test]
fn a_folder_is_found_by_key_server_name_or_label() {
    let catalogue = Catalogue::resolve(&tum_account(), &tum_listing());
    assert_eq!(catalogue.find("junk").unwrap().server, "Junk Email");
    assert_eq!(catalogue.find("JUNK EMAIL").unwrap().server, "Junk Email");
    assert_eq!(catalogue.find("sent").unwrap().server, "Sent Items");
    assert_eq!(
        catalogue.find("inbox/REIMBURSEMENTS").unwrap().key,
        "INBOX/reimbursements"
    );
    assert!(catalogue.find("Nowhere").is_none());
    assert_eq!(
        catalogue.storage_for("Calendar").unwrap(),
        ("Calendar".to_string(), "Calendar".to_string()),
        "an ignored folder is stored under its own key"
    );
    assert!(
        catalogue.storage_for("Created Since").is_err(),
        "a folder the last LIST did not have"
    );
}

/// A label reads decoded while the key and the SELECT name stay as sent.
#[test]
fn a_utf7_name_is_decoded_for_the_label_only() {
    let catalogue = Catalogue::resolve(&AccountConfig::default(), &[listed("Gel&APY-scht", &[])]);
    let folder = catalogue.find("Gelöscht").unwrap();
    assert_eq!(folder.label, "Gelöscht");
    assert_eq!(folder.server, "Gel&APY-scht");
    assert_eq!(folder.key, "Gel&APY-scht");
}

/// A Graph account is Exchange, its roles come from the well-known folders'
/// attributes, and a localized Inbox is still the inbox.
#[test]
fn a_graph_listing_resolves_its_roles_from_the_well_known_folders() {
    let account = AccountConfig {
        auth_method: AuthMethod::Graph,
        ..Default::default()
    };
    let catalogue = Catalogue::resolve(
        &account,
        &[
            listed("Posteingang", &["\\Inbox"]),
            listed("Posteingang/Projekte", &[]),
            listed("Gesendete Elemente", &["\\Sent"]),
            listed("Kalender", &[]),
            listed("Calendar", &[]),
        ],
    );
    let inbox = catalogue.role(FolderRole::Inbox).unwrap();
    assert_eq!(
        (inbox.key.as_str(), inbox.server.as_str()),
        ("inbox", "Posteingang")
    );
    assert_eq!(
        policy_of(&catalogue, "Posteingang/Projekte"),
        FolderPolicy::Sync
    );
    assert_eq!(policy_of(&catalogue, "Calendar"), FolderPolicy::Ignore);
}

/// Acceptance: a folder that exists on the server is reachable by
/// `mp sync --mailbox` without a config edit, whatever its policy; one the
/// server does not list, or cannot select, is refused with the names it does
/// list.
#[test]
fn mp_sync_mailbox_reaches_any_listed_folder() {
    let account = tum_account();
    let catalogue = Catalogue::resolve(&account, &tum_listing());
    let target = |name: &str| catalogue.sync_target(&account, name);

    let spesen = target("spesen").unwrap();
    assert_eq!(
        (spesen.role.as_str(), spesen.server_name.as_str()),
        ("Spesen", "Spesen")
    );
    let reimbursements = target("INBOX/reimbursements").unwrap();
    assert_eq!(reimbursements.role.as_str(), "INBOX/reimbursements");
    let junk = target("junk").unwrap();
    assert_eq!(junk.server_name, "Junk Email");
    assert_eq!(
        junk.window, None,
        "an explicit sync downloads the folder in full"
    );
    assert_eq!(target("inbox").unwrap().role, MailboxRole::Inbox);

    let refused = target("Nowhere").unwrap_err();
    assert!(refused.contains("Archive1"), "{refused}");
    let proton = proton_account();
    let proton_catalogue = Catalogue::resolve(&proton, &proton_listing());
    assert!(
        proton_catalogue.sync_target(&proton, "Labels").is_err(),
        "\\Noselect"
    );
    assert!(
        proton_catalogue.sync_target(&proton, "Drafts").is_err(),
        "shadows the local Drafts"
    );
}
