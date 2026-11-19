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
