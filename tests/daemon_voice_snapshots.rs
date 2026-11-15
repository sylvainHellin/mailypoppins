//! Draft snapshots for the voice lane, end to end through the daemon.
//!
//! A draft the daemon creates for an account `[voice_snapshots] accounts`
//! lists is snapshotted at `<data_dir>/voice-snapshots/<account>/<id>.json`,
//! and the send that delivers it adds what went out. Routed side only: the
//! pre-daemon oracle wrote no snapshots, so there is nothing to compare.

mod support;

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;
use tempfile::TempDir;

use mailypoppins::daemon::methods::send::FAKE_TRANSPORT_ENV;
use support::parity::{pin_font_size, DaemonFixture};
use support::send_fixture as fixture;

const ACCOUNT: &str = fixture::ACCOUNT;

/// A seeded root whose config lists `accounts` for snapshots (or leaves the
/// table out when `None`), and a daemon over it with the fake transport.
///
/// The account's drafts mirror is off: a new draft is otherwise linked to a
/// server copy, and its send asks a Drafts folder the fixture has no server
/// for, which is the mirror's business and not this suite's.
///
/// The field order is the drop order: the daemon dies before its root goes.
struct Root {
    daemon: DaemonFixture,
    tmp: TempDir,
}

impl Root {
    fn start(accounts: Option<&[&str]>, before: impl FnOnce(&Path)) -> Root {
        let tmp = TempDir::new().expect("a temporary root");
        fixture::seed(tmp.path());
        pin_font_size(tmp.path());
        let path = tmp.path().join("config.toml");
        let from = format!("default_from = \"{ACCOUNT}@example.com\"\n");
        let config = std::fs::read_to_string(&path).unwrap();
        assert!(
            config.contains(&from),
            "the fixture names {ACCOUNT}'s address"
        );
        let config = config.replacen(
            &from,
            &format!("{from}\n[accounts.drafts]\nmirror = false\n"),
            1,
        );
        std::fs::write(&path, config).unwrap();
        if let Some(accounts) = accounts {
            let existing = std::fs::read_to_string(&path).unwrap();
            let list: Vec<String> = accounts.iter().map(|a| format!("{a:?}")).collect();
            std::fs::write(
                &path,
                format!(
                    "[voice_snapshots]\naccounts = [{}]\n\n{existing}",
                    list.join(", ")
                ),
            )
            .unwrap();
        }
        before(tmp.path());
        let hook = fixture::fake_transport(&fixture::transport_log(tmp.path()));
        let daemon = DaemonFixture::start_with(tmp.path(), None, &[(FAKE_TRANSPORT_ENV, &hook)]);
        Root { daemon, tmp }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    fn mp(&self, args: &[&str]) -> Output {
        let out = self.daemon.mp_routed(args);
        assert_eq!(
            out.status.code(),
            Some(0),
            "`mp {}`\nstdout: {}\nstderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    /// Run a command that creates a draft and hand back its selector and id.
    fn create(&self, args: &[&str]) -> (String, String) {
        let out = self.mp(args);
        let selector = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let id = selector
            .rsplit('/')
            .next()
            .filter(|_| selector.starts_with(&format!("mp://{ACCOUNT}/drafts/")))
            .unwrap_or_else(|| {
                panic!(
                    "`mp {}` printed no draft selector: {selector:?}",
                    args.join(" ")
                )
            })
            .to_string();
        (selector, id)
    }

    fn draft_path(&self, selector: &str) -> PathBuf {
        let out = self.mp(&["path", selector]);
        PathBuf::from(String::from_utf8_lossy(&out.stdout).trim())
    }

    fn snapshot_file(&self, id: &str) -> PathBuf {
        self.root()
            .join("voice-snapshots")
            .join(ACCOUNT)
            .join(format!("{id}.json"))
    }

    fn snapshot(&self, id: &str) -> Value {
        let path = self.snapshot_file(id);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("the snapshot {} is on disk: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is JSON: {e}", path.display()))
    }
}

/// Give a draft the recipient, subject and body a person would, and approve
/// it, so `mp send` takes it.
fn fill_in_and_approve(root: &Root, selector: &str, path: &Path, text: &str) {
    mailypoppins::draft::rewrite_draft_recipients(
        path,
        &mailypoppins::draft::DraftRecipientEdit {
            to: fixture::TO.to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Voice".to_string(),
        },
    )
    .unwrap();
    let file = std::fs::read_to_string(path).unwrap();
    let end = file[3..].find("\n---\n").expect("a frontmatter block") + 3 + "\n---\n".len();
    std::fs::write(path, format!("{}\n{text}\n{}", &file[..end], &file[end..])).unwrap();
    root.mp(&["mark-approved", selector]);
}

/// `mp new` on a listed account writes the snapshot of the draft as created.
#[test]
fn mp_new_snapshots_a_draft_of_a_listed_account() {
    let root = Root::start(Some(&[ACCOUNT]), |_| {});
    let (selector, id) = root.create(&["-A", ACCOUNT, "new", "voice-note"]);
    let path = root.draft_path(&selector);
    let draft = mailypoppins::draft::parse_email_draft(&path).unwrap();

    let snapshot = root.snapshot(&id);
    assert_eq!(snapshot["draft_id"], id.as_str());
    assert_eq!(snapshot["account"], ACCOUNT);
    assert!(snapshot["created"]
        .as_str()
        .is_some_and(|c| { chrono::DateTime::parse_from_rfc3339(c).is_ok() }));
    assert_eq!(snapshot["subject"], "");
    assert_eq!(snapshot["to_domain"], Value::Null);
    assert_eq!(snapshot["body"], draft.body_markdown.as_str());
    assert!(snapshot.get("sent_message_id").is_none());
}

/// `mp reply` creates a draft through another method, and it is snapshotted
/// too, with the domain of the message it answers.
#[test]
fn mp_reply_snapshots_its_draft_too() {
    let root = Root::start(Some(&[ACCOUNT]), |_| {});
    let (_, id) = root.create(&["-A", ACCOUNT, "reply", support::read_fixture::BERICHT]);
    let snapshot = root.snapshot(&id);
    assert_eq!(snapshot["draft_id"], id.as_str());
    let domain = snapshot["to_domain"]
        .as_str()
        .expect("a reply has a recipient");
    assert!(!domain.contains('@'), "a domain, not an address: {domain}");
}

/// `contact.vcard_draft` writes a draft through `draft.create` but outside
/// the draft family's dispatch, and it is snapshotted as well.
#[tokio::test]
async fn a_vcard_draft_is_snapshotted_too() {
    use mp_client::{ClientInfo, ClientKind, Connection, Identity};

    let root = Root::start(Some(&[ACCOUNT]), |_| {});
    let mut conn = Connection::connect(&support::parity::socket_path(root.root()))
        .await
        .expect("the daemon socket answers");
    conn.initialize(
        ClientInfo {
            kind: ClientKind::Cli,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
        },
        Identity {
            data_dir: root.root().to_path_buf(),
            config_dir: root.root().to_path_buf(),
        },
        &[],
        &[],
    )
    .await
    .expect("a compatible handshake");
    let answer = conn
        .call(
            "contact.vcard_draft",
            serde_json::json!({
                "account": ACCOUNT,
                "name": "vcard-jane",
                "address": "jane@example.com",
                "display_name": "Doe, Jane",
            }),
        )
        .await
        .expect("contact.vcard_draft answers");
    let id = answer["draft"]["id"]
        .as_str()
        .expect("the draft's id")
        .to_string();

    let snapshot = root.snapshot(&id);
    assert_eq!(snapshot["draft_id"], id.as_str());
    assert_eq!(snapshot["to_domain"], "example.com");
    assert!(!snapshot.to_string().contains("jane@"), "{snapshot}");
}

/// An account the setting does not list is not snapshotted, and neither is
/// one when the table is absent and the account is not `tum`.
#[test]
fn an_unlisted_account_gets_no_snapshot() {
    for accounts in [Some(&[fixture::OTHER_ACCOUNT][..]), None] {
        let root = Root::start(accounts, |_| {});
        let (_, id) = root.create(&["-A", ACCOUNT, "new", "voice-note"]);
        assert!(
            !root.snapshot_file(&id).exists(),
            "{accounts:?}: {} was written",
            root.snapshot_file(&id).display()
        );
    }
}

/// `mp send` adds the Message-ID, the time and the body that went out to the
/// snapshot, and keeps the body as created, though the draft file is retired.
#[test]
fn mp_send_completes_the_snapshot_before_the_draft_is_retired() {
    let root = Root::start(Some(&[ACCOUNT]), |_| {});
    let (selector, id) = root.create(&["-A", ACCOUNT, "new", "voice-note"]);
    let path = root.draft_path(&selector);
    let created = root.snapshot(&id);

    fill_in_and_approve(&root, &selector, &path, "Edited by hand before sending.");
    let sent_body = mailypoppins::draft::parse_email_draft(&path)
        .unwrap()
        .body_markdown;
    root.mp(&["send", &selector, "-y"]);
    assert!(!path.exists(), "the fully delivered draft was retired");

    let snapshot = root.snapshot(&id);
    assert_eq!(
        snapshot["body"], created["body"],
        "the body as created stays"
    );
    assert_eq!(snapshot["created"], created["created"]);
    let mid = snapshot["sent_message_id"].as_str().expect("a Message-ID");
    assert!(mid.starts_with('<') && mid.ends_with('>'), "{mid}");
    let events = fixture::transport_events(&fixture::transport_log(root.root()));
    assert!(
        events.iter().any(|event| matches!(
            event,
            fixture::TransportEvent::Submit { message_id, .. } if message_id == mid
        )),
        "the recorded id is the one submitted: {events:?}"
    );
    assert!(snapshot["sent_at"]
        .as_str()
        .is_some_and(|at| chrono::DateTime::parse_from_rfc3339(at).is_ok()));
    assert_eq!(snapshot["sent_body"], sent_body.as_str());
    assert!(sent_body.contains("Edited by hand before sending."));
}

/// A snapshot root that cannot be written blocks neither the draft nor the
/// send: both succeed, and there is no snapshot.
#[test]
fn a_snapshot_that_cannot_be_written_blocks_neither_draft_nor_send() {
    let root = Root::start(Some(&[ACCOUNT]), |root| {
        std::fs::write(root.join("voice-snapshots"), "not a directory").unwrap();
    });
    let (selector, _) = root.create(&["-A", ACCOUNT, "new", "voice-note"]);
    let path = root.draft_path(&selector);
    fill_in_and_approve(&root, &selector, &path, "Sent anyway.");
    root.mp(&["send", &selector, "-y"]);
    assert!(!path.exists(), "the send went out and retired the draft");
    assert!(root.root().join("voice-snapshots").is_file());
}
