//! The seeded store the Phase 4 read slice is measured against (plan P4-U3).
//!
//! `mp show`, `mp list-messages`, `mp dump-mailbox --json` and
//! `mp search --local` all answer from one store, so they are all compared
//! against one fixture. It is built here rather than in the test file so the
//! parity assertions, the raw JSON-RPC assertions and the routed twins of the
//! three legacy suites read the same rows.
//!
//! # The sandbox layout
//!
//! [`support::parity::sandbox_env`] points `HOME`, `MAILYPOPPINS_DATA_DIR` and
//! `MAILYPOPPINS_CONFIG_DIR` all at one root, so [`seed`] writes
//! `<root>/config.toml` and `<root>/accounts/<name>/store.sqlite3`. The daemon
//! reads its configuration at startup, so a fixture is seeded **before**
//! `DaemonFixture::start`.
//!
//! Nothing here uses `mailypoppins::config::store_path`, which reads the
//! process environment: these tests run in one process, each with its own root,
//! and a process-wide variable would let two fixtures write into each other.
//!
//! # What the fixture has, and why
//!
//! Four accounts and seven messages, each axis of the read slice present once:
//!
//! | axis | where |
//! |---|---|
//! | non-ASCII subject and body | `alpha inbox/1` "Bericht über Anträge" |
//! | attachments, and two of them | `alpha inbox/1` (`agenda.txt`, `notes.pdf`) |
//! | every flag axis, `\Flagged` included | `alpha inbox/1` |
//! | a `Cc:` header | `alpha inbox/1` |
//! | unread | `alpha inbox/2` |
//! | a calendar invitation | `alpha inbox/2` |
//! | HTML-only mail, read as flattened text | `alpha inbox/3` |
//! | no `Message-ID:` and no usable `Date:` | `alpha inbox/4` |
//! | no stored body (the `mp show` degrade) | `alpha inbox/4` |
//! | a second mailbox | `alpha sent/1` |
//! | a third mailbox whose id sorts before the others | `alpha Team/Reports/1` |
//! | a second account | `beta inbox/1` |
//! | a configured account with no store at all | `delta` |
//!
//! `delta` is the account `mp dump-mailbox` must skip in silence and every
//! `message.*` method must refuse with `account_not_ready`; it has no
//! `accounts/delta` directory, which is the only way to produce that state
//! without a runtime.
//!
//! The `Team/Reports` mailbox earns its place twice: the mailbox id is not a
//! role, and `"Team/Reports" < "inbox" < "sent"` in byte order, which is the
//! order `mp dump-mailbox` emits its records in and the one a per-mailbox
//! query has to reproduce.
//!
//! # Search
//!
//! The bodies are written so `mp search --local ledger` has a determined
//! ranking: `alpha inbox/3` carries "Ledger" in its *subject* and
//! `alpha inbox/1` and `alpha sent/1` carry "ledger" in their *bodies*, so the
//! subject hit outranks both (`src/store/search.rs`'s bm25 weights) and
//! `--mailbox sent` narrows to one. "aardvark" is the query with no hits.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use mailypoppins::ingest::{ingest_message, IngestInput};
use mailypoppins::parse::{AttachmentData, FetchedEmail};
use mailypoppins::store::{BlobStore, Store};
use mailypoppins::types::MessageFlags;

/// The first configured account, which every `-A`-less command answers about.
pub const ACCOUNT: &str = "alpha";

/// The second account, reachable only by naming it.
pub const OTHER_ACCOUNT: &str = "beta";

/// The configured account with no store on disk.
pub const STORELESS_ACCOUNT: &str = "delta";

/// An account name no configuration carries, for the `account_unknown` path.
pub const UNKNOWN_ACCOUNT: &str = "zeta";

/// The mailboxes of [`ACCOUNT`] that hold rows, in the byte order
/// `mp dump-mailbox` sorts them by.
pub const MAILBOXES: [&str; 3] = ["Team/Reports", "inbox", "sent"];

/// The `Message-ID` of the message with attachments, flags, a `Cc:` and a
/// non-ASCII subject.
pub const BERICHT: &str = "bericht@example.com";

/// The `Message-ID` of the unread invitation.
pub const KICKOFF: &str = "kickoff@example.com";

/// The `Message-ID` of the HTML-only message.
pub const MARKUP: &str = "markup@example.com";

/// The `Message-ID` of the sent copy.
pub const SENT: &str = "re-bericht@example.com";

/// The `Message-ID` of the message only [`OTHER_ACCOUNT`] holds.
pub const ONLY_IN_BETA: &str = "only-in-beta@example.com";

/// The configuration the fixture writes, four accounts in a fixed order.
///
/// `alpha` is first, so it is the default account. The `extra` mailbox is
/// declared the way `mp sync` declares a non-role mailbox: the server name is
/// the mailbox key, verbatim (#0064).
pub const CONFIG: &str = r#"
[[accounts]]
name = "alpha"
default_from = "alpha@example.com"

[accounts.mailboxes.inbox]
server = "INBOX"

[[accounts.mailboxes.extra]]
server = "Team/Reports"

[[accounts]]
name = "beta"
default_from = "beta@example.com"

[[accounts]]
name = "delta"
default_from = "delta@example.com"
"#;

/// `<root>/accounts/<account>`.
pub fn account_dir(root: &Path, account: &str) -> PathBuf {
    root.join("accounts").join(account)
}

/// One account's fixture store, opened by the test itself, so an expectation
/// comes from the rows the CLI reads rather than from a copy of them.
pub fn store(root: &Path, account: &str) -> Store {
    Store::open(account_dir(root, account).join("store.sqlite3")).expect("open the fixture store")
}

/// Write the configuration and the stores under `root`.
///
/// Call it before starting a daemon against the same root: the daemon loads
/// `config.toml` once, at startup.
pub fn seed(root: &Path) {
    fs::create_dir_all(root).unwrap_or_else(|e| panic!("create {}: {e}", root.display()));
    fs::write(root.join("config.toml"), CONFIG).expect("write config.toml");
    seed_alpha(root);
    seed_beta(root);
}

fn seed_alpha(root: &Path) {
    // Every axis at once: read, answered, forwarded *and* flagged, two
    // attachments, a Cc, and a subject and body that are not ASCII. The fourth
    // flag is the one the wire shape does not carry.
    let mut bericht = email(
        "Ivana Hečimović <ivana@example.com>",
        "Sylvain Hellin <sylvain@example.com>",
        "Bericht über Anträge",
        "Thu, 2 Jul 2026 13:57:30 +0200",
        "der quarterly ledger ist beigefügt\n",
    );
    bericht.message_id = Some(format!("<{BERICHT}>"));
    bericht.cc = Some("\"Prof. Petzold\" <petzold@example.com>".to_string());
    bericht.flags = MessageFlags {
        seen: true,
        answered: true,
        forwarded: true,
        flagged: true,
    };
    bericht.has_attachments = true;
    bericht.attachments = vec![
        AttachmentData {
            filename: "notes.pdf".to_string(),
            content: b"%PDF-1.4 notes".to_vec(),
            content_id: None,
        },
        AttachmentData {
            filename: "agenda.txt".to_string(),
            content: b"abc".to_vec(),
            content_id: None,
        },
    ];
    ingest(root, ACCOUNT, "inbox", 1, &bericht);

    // Unread, and an invitation: the two facts a listing and a dump each
    // report in their own column.
    let mut kickoff = email(
        "Organizer <organizer@example.com>",
        "sylvain@example.com",
        "Kickoff",
        "Wed, 1 Jul 2026 09:00:00 +0200",
        "the kickoff agenda follows\n",
    );
    kickoff.message_id = Some(format!("<{KICKOFF}>"));
    kickoff.calendar_ics = Some(
        b"BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:evt-1\r\nSUMMARY:Kickoff\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            .to_vec(),
    );
    ingest(root, ACCOUNT, "inbox", 2, &kickoff);

    // HTML-only: the markup is kept as its own blob and the body is the
    // flattened text the fetcher derived, which is what a reader is shown
    // (RD-03). The subject carries the search term whose rank must beat a body
    // hit.
    let mut markup = email(
        "designer@example.com",
        "sylvain@example.com",
        "Ledger in HTML",
        "Tue, 30 Jun 2026 08:15:00 +0200",
        "quarterly summary follows\n",
    );
    markup.message_id = Some(format!("<{MARKUP}>"));
    markup.html_body =
        Some("<html><body><p>quarterly summary follows</p></body></html>".to_string());
    markup.flags = MessageFlags::seen(true);
    ingest(root, ACCOUNT, "inbox", 3, &markup);

    // No `Message-ID:`, no usable `Date:` and no body: three degrades in one
    // row. Ingest synthesises the id, `date_sort` is the empty string, and
    // `mp show` prints the "no stored body" sentence rather than an error.
    let undated = email(
        "undated@example.com",
        "sylvain@example.com",
        "No date header",
        "(unknown date)",
        "",
    );
    ingest(root, ACCOUNT, "inbox", 4, &undated);

    // A second mailbox, with an attachment of its own and a body that shares
    // the search term, so `--mailbox` narrows a search to something.
    let mut reply = email(
        "sylvain@example.com",
        "ivana@example.com",
        "Re: Bericht über Anträge",
        "Mon, 29 Jun 2026 12:00:00 +0000",
        "the ledger is signed\n",
    );
    reply.message_id = Some(format!("<{SENT}>"));
    reply.flags = MessageFlags::seen(true);
    reply.has_attachments = true;
    reply.attachments = vec![AttachmentData {
        filename: "report.pdf".to_string(),
        content: b"%PDF-1.4".to_vec(),
        content_id: None,
    }];
    ingest(root, ACCOUNT, "sent", 1, &reply);

    // A third mailbox whose id is neither a role nor lowercase.
    let mut weekly = email(
        "bot@example.com",
        "sylvain@example.com",
        "Weekly",
        "Sun, 28 Jun 2026 07:00:00 +0000",
        "nothing to report\n",
    );
    weekly.message_id = Some("<weekly@example.com>".to_string());
    weekly.flags = MessageFlags::seen(true);
    ingest(root, ACCOUNT, "Team/Reports", 1, &weekly);
}

fn seed_beta(root: &Path) {
    let mut only = email(
        "someone@example.com",
        "beta@example.com",
        "Only-In-Beta",
        "Fri, 1 May 2026 05:00:00 +0000",
        "beta body\n",
    );
    only.message_id = Some(format!("<{ONLY_IN_BETA}>"));
    ingest(root, OTHER_ACCOUNT, "inbox", 1, &only);
}

/// One message, as a sync would hand it to ingest.
pub fn email(from: &str, to: &str, subject: &str, date: &str, body: &str) -> FetchedEmail {
    FetchedEmail {
        from: from.to_string(),
        to: to.to_string(),
        cc: None,
        reply_to: None,
        bcc: None,
        subject: subject.to_string(),
        date: date.to_string(),
        body_text: body.to_string(),
        html_body: None,
        has_attachments: false,
        message_id: None,
        attachments: Vec::new(),
        flags: MessageFlags::default(),
        calendar_ics: None,
        event: None,
    }
}

/// Ingest one message into `account`'s store under `root`.
pub fn ingest(root: &Path, account: &str, mailbox: &str, uid: i64, message: &FetchedEmail) {
    let dir = account_dir(root, account);
    fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    let store = Store::open(dir.join("store.sqlite3")).expect("open the fixture store");
    let blobs = BlobStore::new(dir.join("blobs"));
    ingest_message(
        &store,
        &blobs,
        &IngestInput {
            account,
            mailbox,
            uid,
            email: message,
            raw: None,
        },
    )
    .expect("ingest the fixture message");
}
