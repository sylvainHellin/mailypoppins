//! One account served over the wire by the two fake servers (PERSO-210).
//!
//! The account [`ACCOUNT`] submits to [`RecordingSmtpServer`] over STARTTLS
//! and syncs, mirrors drafts and files its Sent copies on
//! [`RejectingImapServer`], both on localhost with `accept_invalid_certs`,
//! with its two passwords in the sandbox's `secrets.enc`. A test seeds the
//! drafts and the server folders it needs, then starts the daemon: nothing
//! in the daemon is told it is under test, so the send, the drain, the sync
//! and the watch it runs are the ones a user's daemon runs.
//!
//! The account's scheduled sync is off (`sync_interval_secs = 0`), so the
//! only server traffic is what the test asks for and what a send or a draft
//! change brings with it.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use tempfile::TempDir;

use super::fake_imap::{LoginPolicy, RejectingImapServer};
use super::fake_smtp::RecordingSmtpServer;
use super::parity::DaemonFixture;

/// The account's name.
pub const ACCOUNT: &str = "wire";

/// Its address, the envelope sender of every message it sends.
pub const ADDRESS: &str = "wire@example.com";

/// The two passwords the sandbox stores.
pub const SMTP_PASSWORD: &str = "smtp-secret";
pub const IMAP_PASSWORD: &str = "imap-secret";

/// The server folders the account's mailbox roles name.
pub const INBOX: &str = "INBOX";
pub const SENT: &str = "Sent";
pub const DRAFTS: &str = "Drafts";
pub const ARCHIVE: &str = "Archive";
pub const TRASH: &str = "Trash";

/// A root with the account, the two servers and, once started, a daemon.
pub struct WireRoot {
    pub smtp: RecordingSmtpServer,
    pub imap: RejectingImapServer,
    tmp: TempDir,
    daemon: Option<DaemonFixture>,
}

impl WireRoot {
    /// Start both servers, the IMAP one holding the account's folders,
    /// empty, and write the configuration and the passwords. The daemon is
    /// not started: seed drafts and mail first, then [`WireRoot::start`].
    pub fn new() -> WireRoot {
        let smtp = RecordingSmtpServer::start();
        let imap = RejectingImapServer::start_with(LoginPolicy::Accept);
        imap.set_folders(Vec::new());
        for (folder, uidvalidity) in [
            (INBOX, 11),
            (SENT, 12),
            (DRAFTS, 13),
            (ARCHIVE, 14),
            (TRASH, 15),
        ] {
            imap.set_mail(folder, uidvalidity, Vec::new());
        }
        let tmp = TempDir::new().expect("a temporary wire root");
        let root = WireRoot {
            smtp,
            imap,
            tmp,
            daemon: None,
        };
        root.write_config("");
        root.store_passwords();
        // A store, as an account that has synced once has: the daemon starts
        // no runtime for an account without one.
        fs::create_dir_all(root.account_dir()).expect("create the account directory");
        drop(root.store());
        root
    }

    /// Rewrite `config.toml` with `extra` appended under the account's
    /// `[accounts.imap]` table; before [`WireRoot::start`] only.
    pub fn write_config(&self, extra_imap: &str) {
        let config = format!(
            "[[accounts]]\nname = \"{ACCOUNT}\"\ndefault_from = \"{ADDRESS}\"\n\n\
             [accounts.smtp]\nhost = \"127.0.0.1\"\nport = {}\nusername = \"{ADDRESS}\"\n\
             accept_invalid_certs = true\n\n\
             [accounts.imap]\nhost = \"127.0.0.1\"\nport = {}\nusername = \"{ADDRESS}\"\n\
             accept_invalid_certs = true\nsync_interval_secs = 0\n{extra_imap}\n\
             [accounts.mailboxes.inbox]\nserver = \"{INBOX}\"\n\
             [accounts.mailboxes.sent]\nserver = \"{SENT}\"\n\
             [accounts.mailboxes.drafts]\nserver = \"{DRAFTS}\"\n\
             [accounts.mailboxes.archive]\nserver = \"{ARCHIVE}\"\n\
             [accounts.mailboxes.trash]\nserver = \"{TRASH}\"\n",
            self.smtp.port(),
            self.imap.port(),
        );
        fs::write(self.root().join("config.toml"), config).expect("write config.toml");
    }

    fn store_passwords(&self) {
        use mailypoppins::secrets::{EncryptedFileBackend, SecretsBackend};
        let secrets = EncryptedFileBackend::open(self.root().join("secrets.enc"))
            .expect("the sandbox's secrets file opens");
        secrets
            .set(&format!("smtp-password-{ACCOUNT}"), SMTP_PASSWORD)
            .expect("store the SMTP password");
        secrets
            .set(&format!("imap-password-{ACCOUNT}"), IMAP_PASSWORD)
            .expect("store the IMAP password");
    }

    /// Start the daemon with `env` on top of the sandbox.
    pub fn start(&mut self, env: &[(&str, &str)]) {
        assert!(self.daemon.is_none(), "the daemon is already running");
        self.daemon = Some(DaemonFixture::start_with(self.tmp.path(), None, env));
    }

    /// Stop the daemon, waiting until it is gone.
    pub fn stop(&mut self) {
        if let Some(daemon) = self.daemon.take() {
            daemon.stop();
        }
    }

    pub fn root(&self) -> &Path {
        self.tmp.path()
    }

    /// `<root>/accounts/wire`.
    pub fn account_dir(&self) -> PathBuf {
        self.root().join("accounts").join(ACCOUNT)
    }

    /// `<root>/accounts/wire/drafts`.
    pub fn drafts_dir(&self) -> PathBuf {
        self.account_dir().join("drafts")
    }

    /// Write an approved draft `id` to `to` (comma-separated), as
    /// `<id>.md`, and answer its selector.
    pub fn draft(&self, id: &str, to: &str, subject: &str, body: &str) -> String {
        let dir = self.drafts_dir();
        fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
        let path = dir.join(format!("{id}.md"));
        let text = format!(
            "---\nid: {id}\nto: {to}\ncc:\nbcc:\nsubject: \"{subject}\"\nstatus: approved\n\
             from: {ADDRESS}\ndate: 2026-07-01 09:00\n---\n\n{body}"
        );
        fs::write(&path, text).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        format!("mp://{ACCOUNT}/drafts/{id}")
    }

    /// The client, required to go through the daemon.
    pub fn mp(&self, args: &[&str]) -> Output {
        self.daemon
            .as_ref()
            .expect("the daemon runs")
            .mp_routed(args)
    }

    /// The account's store, opened by the test.
    pub fn store(&self) -> mailypoppins::store::Store {
        mailypoppins::store::Store::open(self.account_dir().join("store.sqlite3"))
            .expect("open the account's store")
    }
}

impl WireRoot {
    /// `(uid, Message-ID)` of each message the local store lists in the
    /// local mailbox `mailbox` (`inbox`, `archive`, ...), by ascending UID.
    pub fn local(&self, mailbox: &str) -> Vec<(i64, String)> {
        let mut rows: Vec<(i64, String)> =
            mailypoppins::store::read::list_mailbox(&self.store(), ACCOUNT, mailbox)
                .unwrap_or_else(|e| panic!("list {mailbox}: {e:#}"))
                .into_iter()
                .map(|row| (row.uid, row.message_id))
                .collect();
        rows.sort();
        rows
    }

    /// What the server's Sent folder holds once the account's outbox owes
    /// no APPEND (no row open, none filing), so no copy can be filed after
    /// this reads it: what a test counts copies with. A row whose APPEND is
    /// still to come is waited for, up to the deadline.
    pub fn sent_once_settled(&self) -> Vec<super::fake_imap::HeldMessage> {
        wait_for("the outbox to owe no APPEND", || {
            let counts = mailypoppins::outbox::counts(&self.store(), ACCOUNT).expect("the counts");
            counts.open == 0 && counts.filing == 0
        });
        self.imap.contents(SENT)
    }

    /// `mp sync`, asserted to succeed.
    pub fn sync(&self) -> Output {
        let out = self.mp(&["sync"]);
        assert_ok(&out, "mp sync");
        out
    }
}

impl Default for WireRoot {
    fn default() -> Self {
        WireRoot::new()
    }
}

/// The two fake servers, for an account of another fixture to be moved
/// onto (PERSO-331): the send, admin and calendar fixtures seed accounts
/// that configure no server, and a test that needs a send to go out points
/// one of them at these with [`WireServers::wire`] before its daemon starts.
///
/// The IMAP server holds the five folders [`WireRoot`]'s does, empty. An
/// account wired here keeps its own store, drafts and outbox, so a test
/// asserts on what its fixture seeded; only the transport is the wire's.
pub struct WireServers {
    pub smtp: RecordingSmtpServer,
    pub imap: RejectingImapServer,
}

impl WireServers {
    /// Start both servers, the IMAP one with empty folders.
    pub fn start() -> WireServers {
        let smtp = RecordingSmtpServer::start();
        let imap = RejectingImapServer::start_with(LoginPolicy::Accept);
        imap.set_folders(Vec::new());
        for (folder, uidvalidity) in [
            (INBOX, 11),
            (SENT, 12),
            (DRAFTS, 13),
            (ARCHIVE, 14),
            (TRASH, 15),
        ] {
            imap.set_mail(folder, uidvalidity, Vec::new());
        }
        WireServers { smtp, imap }
    }

    /// Point `account` of `<root>/config.toml` at both servers and store
    /// its two passwords in the sandbox's `secrets.enc`; before the daemon
    /// starts, since it reads both once.
    ///
    /// The account submits to the SMTP server and files its Sent copies in
    /// the IMAP server's [`SENT`] folder, as its default sender. Its
    /// scheduled sync is off and so is its drafts mirror: an account of
    /// these fixtures had no server before, so neither ever ran on it. What
    /// reaches the servers is then the sends (the submission, the APPEND
    /// and its dedup search), the watcher's IDLE on [`INBOX`], and the
    /// daemon's first drain of the seeded outbox, which a test waits out
    /// with [`WireServers::await_startup_drain`].
    pub fn wire(&self, root: &Path, account: &str) {
        self.rewrite(root, account, false);
    }

    /// [`WireServers::wire`] with the drafts mirror on, as it is for a user:
    /// a send of a draft the mirror pushed then claims its copy from the
    /// IMAP server's [`DRAFTS`] folder first.
    pub fn wire_mirroring(&self, root: &Path, account: &str) {
        self.rewrite(root, account, true);
    }

    fn rewrite(&self, root: &Path, account: &str, mirror: bool) {
        let path = root.join("config.toml");
        let text =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let mut config: toml::Table = text
            .parse()
            .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
        let accounts = config
            .get_mut("accounts")
            .and_then(toml::Value::as_array_mut)
            .expect("the configuration has accounts");
        let entry = accounts
            .iter_mut()
            .filter_map(toml::Value::as_table_mut)
            .find(|table| table.get("name").and_then(toml::Value::as_str) == Some(account))
            .unwrap_or_else(|| panic!("no account {account} in {}", path.display()));
        let address = entry
            .get("default_from")
            .and_then(toml::Value::as_str)
            .unwrap_or_else(|| panic!("{account} has a default_from"))
            .to_string();
        let server = |port: u16| -> toml::Value {
            let mut table = toml::Table::new();
            table.insert("host".into(), "127.0.0.1".into());
            table.insert("port".into(), i64::from(port).into());
            table.insert("username".into(), address.clone().into());
            table.insert("accept_invalid_certs".into(), true.into());
            toml::Value::Table(table)
        };
        entry.insert("smtp".into(), server(self.smtp.port()));
        let mut imap = match server(self.imap.port()) {
            toml::Value::Table(table) => table,
            _ => unreachable!("built as a table"),
        };
        imap.insert("sync_interval_secs".into(), 0.into());
        entry.insert("imap".into(), toml::Value::Table(imap));
        let mailboxes = entry
            .entry("mailboxes")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .expect("mailboxes is a table");
        mailboxes.entry("sent").or_insert_with(|| {
            let mut sent = toml::Table::new();
            sent.insert("server".into(), SENT.into());
            toml::Value::Table(sent)
        });
        let mut drafts = toml::Table::new();
        drafts.insert("mirror".into(), mirror.into());
        entry.insert("drafts".into(), toml::Value::Table(drafts));
        fs::write(
            &path,
            toml::to_string(&config).expect("serialise the configuration"),
        )
        .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));

        use mailypoppins::secrets::{EncryptedFileBackend, SecretsBackend};
        let secrets = EncryptedFileBackend::open(root.join("secrets.enc"))
            .expect("the sandbox's secrets file opens");
        secrets
            .set(&format!("smtp-password-{account}"), SMTP_PASSWORD)
            .expect("store the SMTP password");
        secrets
            .set(&format!("imap-password-{account}"), IMAP_PASSWORD)
            .expect("store the IMAP password");
    }

    /// Every recipient the SMTP server took a message for, in order, as
    /// `(Message-ID, recipient, acknowledged)`: the client sends one
    /// transaction per recipient, and `acknowledged` is false for a message
    /// the server received and gave no verdict on.
    pub fn submissions(&self) -> Vec<(String, String, bool)> {
        self.smtp
            .deliveries()
            .into_iter()
            .flat_map(|delivery| {
                let message_id = header_of(&delivery.data, "Message-ID");
                delivery
                    .recipients
                    .into_iter()
                    .map(move |to| (message_id.clone(), to, delivery.acknowledged))
            })
            .collect()
    }

    /// The Message-IDs the SMTP server took, each once, in the order it
    /// first took them, those `background` names left out.
    pub fn submitted_message_ids(&self, background: &[&str]) -> Vec<String> {
        let mut ids: Vec<String> = Vec::new();
        for (message_id, _, _) in self.submissions() {
            if !ids.contains(&message_id) && !background.contains(&message_id.as_str()) {
                ids.push(message_id);
            }
        }
        ids
    }

    /// How many copies of `message_id` the server's [`SENT`] folder holds.
    pub fn sent_copies_of(&self, message_id: &str) -> usize {
        self.imap
            .contents(SENT)
            .iter()
            .filter(|held| held.message_id == message_id)
            .count()
    }

    /// How many `SEARCH`es the IMAP server ran in its [`SENT`] folder: the
    /// outbox's dedup lookups, the only search an account wired here makes.
    pub fn sent_searches(&self) -> usize {
        self.imap
            .searches()
            .iter()
            .filter(|(folder, _)| folder == SENT)
            .count()
    }

    /// Every recipient the SMTP server refused at `RCPT`, in order, with
    /// the reply it gave.
    pub fn refused_recipients(&self) -> Vec<(String, String)> {
        self.smtp
            .rcpts()
            .into_iter()
            .filter_map(|rcpt| rcpt.refusal.map(|reply| (rcpt.address, reply)))
            .collect()
    }

    /// Whether nothing reached either server: no recipient named to the
    /// SMTP server and no copy filed in [`SENT`].
    pub fn untouched(&self) -> bool {
        self.smtp.rcpts().is_empty() && self.imap.contents(SENT).is_empty()
    }

    /// Whether nothing but the messages `background` names reached either
    /// server: no other message submitted or filed in [`SENT`], and no
    /// recipient refused, since a refusal names no message.
    pub fn untouched_but_for(&self, background: &[&str]) -> bool {
        let ours = |message_id: &str| !background.contains(&message_id);
        self.submissions().iter().all(|(mid, _, _)| !ours(mid))
            && self.refused_recipients().is_empty()
            && self
                .imap
                .contents(SENT)
                .iter()
                .all(|held| !ours(&held.message_id))
    }

    /// Wait until the daemon's first drain of `account` under `root` has
    /// sent what the account's seeded outbox owed: no row left to submit and
    /// no copy left to file.
    ///
    /// An account with a server gets a drainer, which a local-only one does
    /// not, and it drains once [`DRAIN_DEBOUNCE`] after the daemon starts
    /// (`src/daemon/runtime/drainer.rs`): a fixture's queued rows go out
    /// then, beside whatever the test does. A test waits for that first,
    /// so its own send never meets a drain already running, and then sets
    /// the servers up for what it tests.
    ///
    /// [`DRAIN_DEBOUNCE`]: mailypoppins::daemon::runtime::drainer::DRAIN_DEBOUNCE
    pub fn await_startup_drain(&self, root: &Path, account: &str) {
        let store = root.join("accounts").join(account).join("store.sqlite3");
        if !store.exists() {
            return;
        }
        wait_for(&format!("the first drain of {account}"), || {
            let store = mailypoppins::store::Store::open(&store).expect("open the account's store");
            let rows = mailypoppins::outbox::unfinished_rows(&store, account).expect("the rows");
            rows.iter().all(|row| {
                !matches!(
                    row.state,
                    mailypoppins::outbox::OutboxState::PendingSend
                        | mailypoppins::outbox::OutboxState::SentPendingAppend
                )
            })
        });
    }
}

/// The header `name` of `raw`, unfolded, empty when it has none.
pub fn header_of(raw: &[u8], name: &str) -> String {
    let (headers, _) = mailparse::parse_headers(raw).expect("headers");
    headers
        .iter()
        .find(|h| h.get_key().eq_ignore_ascii_case(name))
        .map(|h| h.get_value())
        .unwrap_or_default()
}

/// Poll `done` every [`TICK`](super::parity::TICK) until it holds, and fail
/// the test naming `what` after [`DEADLINE`](super::parity::DEADLINE): for
/// what the daemon finishes after the command that started it answered.
pub fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    while !done() {
        assert!(
            start.elapsed() < super::parity::DEADLINE,
            "{what} did not happen within {:?}",
            super::parity::DEADLINE
        );
        std::thread::sleep(super::parity::TICK);
    }
}

/// A message from `from` with Message-ID `<id@example.org>`, for a server
/// folder.
pub fn mail(id: &str, from: &str, subject: &str, body: &str) -> Vec<u8> {
    format!(
        "Message-ID: <{id}@example.org>\r\nFrom: {from}\r\nTo: {ADDRESS}\r\n\
         Subject: {subject}\r\nDate: Wed, 01 Jul 2026 09:00:00 +0000\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
    )
    .into_bytes()
}

/// stdout as text.
pub fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// stderr as text.
pub fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Assert `out` exited 0, showing both streams otherwise.
pub fn assert_ok(out: &Output, what: &str) {
    assert_eq!(
        out.status.code(),
        Some(0),
        "{what}\nstdout:\n{}\nstderr:\n{}",
        stdout(out),
        stderr(out)
    );
}
