//! The seeded root the Phase 4 message-mutation slice is measured against
//! (plan P4-U7).
//!
//! `mp archive`, `mp delete [--force|--sent]`, `mp open` and `mp save [-o]` all
//! answer about one account, so they are all compared against one fixture. It
//! is [`draft_fixture`] - itself [`read_fixture`] plus a drafts directory -
//! with three additions the mutation slice needs and the two earlier slices do
//! not:
//!
//! | axis | where |
//! |---|---|
//! | one message-id in two mailboxes (the ambiguity refusal, and `--mailbox` resolving it) | [`SHARED`] in `inbox/5` and `archive/1` |
//! | two attachments sharing one name (the `_1` rule at save time) | [`TWO_PARTS`] in `inbox/6` |
//! | one draft id in two accounts (the cross-account delete) | [`SHARED_DRAFT`] in both accounts |
//!
//! Building on `draft_fixture` rather than beside it keeps one definition of
//! "the message with attachments" across the three slices, and the additions
//! are made here rather than in `read_fixture` because a new row would move
//! every listing, dump and search expectation the read slice pins.
//!
//! # No server, and what that fixes
//!
//! No account here has credentials, and nothing listens on any port.
//! `mp archive` and `mp delete` of a *received* message resolve the account's
//! backend ([`mailypoppins::ops::Backend::resolve`]) **before** they touch the
//! store, so over this fixture both refuse with
//! [`CREDENTIALS_REFUSAL`] and leave the row exactly where it was. That is the
//! deterministic half of MSG-01 and MSG-02: no network, no DNS text, no
//! timing, and the same bytes from both binaries. The other half - the queued
//! op draining against a real IMAP server, and the rollback when it refuses -
//! needs a server this fixture does not have and stays covered by the
//! `pending_ops` unit tests.
//!
//! # The opener
//!
//! `mp open` hands each materialised file to the system opener, which
//! `parse::open_file_with_system` launches as the bare command `open`, resolved
//! through `PATH`. That resolution *is* the hook: [`opener_env`] puts a
//! recording `open` first on the client's `PATH` and points it at a log, so a
//! test learns which paths were handed over without a viewer ever starting.
//! It needs no new environment variable, it works on the pre-daemon binary
//! too - which is what makes the ATT-01 row comparable at all - and it is a
//! hard safety belt: a test that forgot it would otherwise launch `xdg-open`
//! on the developer's desktop.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use mailypoppins::parse::AttachmentData;

use super::draft_fixture;
use super::read_fixture;

/// The account every command answers about without `-A`.
pub const ACCOUNT: &str = read_fixture::ACCOUNT;

/// The second configured account, reachable only by naming it.
pub const OTHER_ACCOUNT: &str = read_fixture::OTHER_ACCOUNT;

/// The configured account with no store on disk.
pub const STORELESS_ACCOUNT: &str = read_fixture::STORELESS_ACCOUNT;

/// An account name no configuration carries.
pub const UNKNOWN_ACCOUNT: &str = read_fixture::UNKNOWN_ACCOUNT;

/// The message with two attachments, `notes.pdf` then `agenda.txt`.
pub const WITH_ATTACHMENTS: &str = read_fixture::BERICHT;

/// The bytes of [`WITH_ATTACHMENTS`]'s attachments, in the message's own
/// order, which is the order `mp open` and `mp save` walk them in.
pub const ATTACHMENT_FILES: [(&str, &[u8]); 2] =
    [("notes.pdf", b"%PDF-1.4 notes"), ("agenda.txt", b"abc")];

/// The message that carries no attachment at all.
pub const NO_ATTACHMENTS: &str = read_fixture::KICKOFF;

/// The message-id `inbox` and `archive` both hold: the ambiguity every
/// received command has to refuse rather than guess at.
pub const SHARED: &str = "shared@example.com";

/// The message whose two attachments were sent under one name, so
/// materialising them applies the `_1` rule.
pub const TWO_PARTS: &str = "vertraege@example.com";

/// The names the two parts of [`TWO_PARTS`] land under, in that order.
pub const TWO_PARTS_FILES: [(&str, &[u8]); 2] = [
    ("report.pdf", b"first part"),
    ("report_1.pdf", b"second part"),
];

/// A message-id nothing in the fixture holds.
pub const UNKNOWN_MESSAGE: &str = "nope@example.com";

/// The message only [`OTHER_ACCOUNT`] holds, for a cross-account selector.
pub const ONLY_IN_BETA: &str = read_fixture::ONLY_IN_BETA;

/// A valid, deletable draft.
pub const VALID_DRAFT: &str = draft_fixture::VALID;

/// The approved draft, which `mp delete` refuses without `--force`.
pub const APPROVED_DRAFT: &str = draft_fixture::APPROVED;

/// The sent draft, which the `--sent` sweep clears and which needs no
/// `--force`.
pub const SENT_DRAFT: &str = draft_fixture::SENT;

/// A draft id nothing resolves to.
pub const UNKNOWN_DRAFT: &str = draft_fixture::UNKNOWN_ID;

/// The one draft id both accounts hold, so a cross-account selector has a
/// wrong answer available to it.
pub const SHARED_DRAFT: &str = "ba00000000000001";

/// The refusal both received mutations make over an account with no
/// credentials, before they touch the store.
pub const CREDENTIALS_REFUSAL: &str =
    "Secret 'smtp-password-alpha' not found. Run `mp config set-password`.";

/// `<root>/accounts/<account>`.
pub fn account_dir(root: &Path, account: &str) -> PathBuf {
    read_fixture::account_dir(root, account)
}

/// One account's fixture store, opened by the test itself.
pub fn store(root: &Path, account: &str) -> mailypoppins::store::Store {
    read_fixture::store(root, account)
}

/// `<root>/accounts/<account>/drafts`.
pub fn drafts_dir(root: &Path, account: &str) -> PathBuf {
    draft_fixture::drafts_dir(root, account)
}

/// The canonical selector of a draft.
pub fn draft_selector(account: &str, id: &str) -> String {
    draft_fixture::selector(account, id)
}

/// The canonical selector of a received message.
pub fn message_selector(account: &str, mailbox: &str, message_id: &str) -> String {
    format!("mp://{account}/{mailbox}/{message_id}")
}

/// Write the configuration, the stores and the drafts under `root`.
///
/// Call it before starting a daemon against the same root: the daemon loads
/// `config.toml` once, at startup.
pub fn seed(root: &Path) {
    draft_fixture::seed(root);
    seed_ambiguous(root);
    seed_two_parts(root);
    seed_shared_draft(root);
}

/// The same message-id in two mailboxes, with no attachments, so an ambiguity
/// resolved with `--mailbox` fails on the *message* rather than on the
/// resolution - which is how the legacy contract test tells the two apart.
fn seed_ambiguous(root: &Path) {
    let mut copied = read_fixture::email(
        "list@example.com",
        "sylvain@example.com",
        "Copied everywhere",
        "Sat, 27 Jun 2026 06:00:00 +0000",
        "one message, two mailboxes\n",
    );
    copied.message_id = Some(format!("<{SHARED}>"));
    copied.flags = mailypoppins::types::MessageFlags::seen(true);
    read_fixture::ingest(root, ACCOUNT, "inbox", 5, &copied);
    read_fixture::ingest(root, ACCOUNT, "archive", 1, &copied);
}

/// Two parts under one name: ingest keeps both names as sent, and the `_1`
/// suffix is applied where the names become paths (`docs/dump-allow-list.md`).
fn seed_two_parts(root: &Path) {
    let mut contracts = read_fixture::email(
        "legal@example.com",
        "sylvain@example.com",
        "Verträge",
        "Fri, 26 Jun 2026 06:00:00 +0000",
        "both parts are called report.pdf\n",
    );
    contracts.message_id = Some(format!("<{TWO_PARTS}>"));
    contracts.flags = mailypoppins::types::MessageFlags::seen(true);
    contracts.has_attachments = true;
    contracts.attachments = vec![
        AttachmentData {
            filename: "report.pdf".to_string(),
            content: b"first part".to_vec(),
            content_id: None,
        },
        AttachmentData {
            filename: "report.pdf".to_string(),
            content: b"second part".to_vec(),
            content_id: None,
        },
    ];
    read_fixture::ingest(root, ACCOUNT, "inbox", 6, &contracts);
}

/// One draft id in both accounts, so `mp delete mp://beta/drafts/<id>` under
/// the `alpha` default has a wrong file available to delete.
fn seed_shared_draft(root: &Path) {
    for account in [ACCOUNT, OTHER_ACCOUNT] {
        let path = drafts_dir(root, account).join("geteilt.md");
        let document = draft_fixture::document(
            SHARED_DRAFT,
            "robin@example.com",
            "Geteilt",
            "draft",
            "Same id, two accounts.\n",
        );
        fs::write(&path, document).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }
}

// ---------------------------------------------------------------------------
// The opener
// ---------------------------------------------------------------------------

/// The directory holding the recording `open`.
pub fn opener_dir(root: &Path) -> PathBuf {
    root.join("opener")
}

/// The file the recording `open` appends one line to per call.
pub fn opener_log(root: &Path) -> PathBuf {
    opener_dir(root).join("opened.log")
}

/// Write the recording `open` and return the `(PATH, MP_OPEN_LOG)` pair a
/// client must run with.
///
/// `PATH` is prefixed rather than replaced: the script is `/bin/sh`, and a
/// binary that resolves `open` to anything else would be the failure under
/// test rather than a broken fixture.
pub fn opener_env(root: &Path) -> (String, PathBuf) {
    let dir = opener_dir(root);
    fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    let script = dir.join("open");
    fs::write(
        &script,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$MP_OPEN_LOG\"\n",
    )
    .unwrap_or_else(|e| panic!("write {}: {e}", script.display()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700))
            .unwrap_or_else(|e| panic!("chmod {}: {e}", script.display()));
    }
    let inherited = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{inherited}", dir.display());
    (path, opener_log(root))
}

/// Every path the recording `open` was handed, in call order.
pub fn opened_paths(root: &Path) -> Vec<PathBuf> {
    match fs::read_to_string(opener_log(root)) {
        Ok(text) => text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(PathBuf::from)
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Forget every recorded call, so one test can make two assertions.
pub fn clear_opened(root: &Path) {
    let log = opener_log(root);
    match fs::remove_file(&log) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => panic!("clear {}: {e}", log.display()),
    }
}
