//! The seeded root the Phase 4 admin slice is measured against (plan P4-U13).
//!
//! `mp config {init,add-account,show,path,set-password,oauth2-login,
//! reset-secrets}`, `mp contacts {search,rebuild,stats}`, `mp calendar
//! rebuild`, `mp invite accept|tentative|decline`, `mp store gc` and
//! `mp cutover` all answer about one root: one `config.toml`, one secrets
//! file, one token cache, one contact index per account, one store per account
//! and one file-era remnant tree. So they are compared against one fixture,
//! built here rather than in the test file so the parity assertions, the raw
//! JSON-RPC assertions and the state comparisons read the same rows.
//!
//! It is [`send_fixture`] - itself [`draft_fixture`], itself [`read_fixture`] -
//! with five additions the admin slice needs and the earlier slices do not:
//!
//! | axis | where |
//! |---|---|
//! | an invitation this account may RSVP to (`CAL-01`) | [`INVITATION`] |
//! | an invitation this account organised, plus an attendee's reply (`CAL-04`) | [`HOSTED`], [`REPLY`] |
//! | a file-era mailstore and an id-less draft (`MIG-01`) | [`legacy_dir`], [`LEGACY_DRAFT`] |
//! | an OAuth2 token cache to wipe (`ACC-07`) | [`token_cache`] |
//! | a prebuilt contact index, so `built_at` is one value (`CON-01`, `CON-04`) | [`contacts_cache`] |
//!
//! Building on `send_fixture` rather than beside it keeps one definition of
//! "the Graph account" - which `ANO-4` needs twice over, once for
//! `mp send --invite` and once for `mp invite accept` - and one definition of
//! the fake transport, which is what makes a *successful* RSVP reachable
//! without an SMTP server.
//!
//! # Why the contact index is prebuilt, and built by the oracle
//!
//! `mp contacts search` and `mp contacts stats` build the index on demand when
//! no cache is there ([`contacts_cmd::load_or_build`]), so the *first* of the
//! two binaries in a parity row would write the cache and the second would
//! read it. That alone is survivable, but `mp contacts stats` prints
//! `Built at: <timestamp>`, which is the moment the index was built: two runs
//! would print two different lines and the difference would be the fixture's,
//! not the daemon's.
//!
//! So [`seed`] builds the index once, and builds it by running the **oracle**
//! (`mp contacts rebuild`) rather than by calling
//! `mailypoppins::contacts::build_index_for_account` in this process. The
//! library call resolves its paths through `mailypoppins::config::account_dir`,
//! which reads the process environment; these tests run many roots in one
//! process and a process-wide variable would let two fixtures write into each
//! other. The oracle is a child process with the sandbox environment on it,
//! which is exactly the resolution every other row uses, and it needs no
//! daemon (it predates them).
//!
//! # Determinism
//!
//! Everything this fixture writes is fixed, and the two values that are not
//! reproducible are kept out of every parity row rather than masked:
//!
//! - the contact index's `built_at` is fixed by building the cache once, above;
//! - an RSVP mints a `Message-ID`, and the only row that sends one is a
//!   routed-side assertion through the fake transport, never a parity row.
//!
//! `mp store gc` reports byte counts of the seeded blobs, which are literals
//! because the seeded attachments are literals.
//!
//! # Why this fixture writes its own `config.toml`
//!
//! [`CONFIG`] is [`read_fixture::CONFIG`] plus [`send_fixture::EXTRA_ACCOUNTS`]
//! with **every SMTP and IMAP port written out**, and that is the one thing
//! this fixture changes about the earlier ones rather than adding to them.
//!
//! `mp config show` prints `smtp.port` from the loaded `GlobalConfig`, and
//! this tree gives that field a serde default (465, and 993 for IMAP) while
//! the `pre-daemon` binary does not: on a configuration that omits the key,
//! the oracle prints `port = 0` and every build since prints `port = 465`.
//! That divergence is older than this slice and is fixed *deliberately* by
//! `docs/daemon-protocol.md` ("effective means after serde defaults, not after
//! the engine's clamps"), so `mp config show` cannot be byte-identical to the
//! oracle on a port-less configuration without contradicting the protocol.
//! Naming the ports takes the difference out of the fixture, which leaves the
//! `ACC-03` row measuring what it is about - that the daemon renders the same
//! configuration - and leaves the divergence itself where it belongs, in the
//! protocol document that chose it.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use mailypoppins::parse::FetchedEmail;
use mailypoppins::store::{BlobStore, Store};
use mailypoppins::types::MessageFlags;

use super::parity::oracle_command;
use super::read_fixture;
use super::send_fixture;

// ---------------------------------------------------------------------------
// The accounts
// ---------------------------------------------------------------------------

/// The default account: first in the file, the one every `-A`-less command
/// answers about, and the one that holds the invitations.
pub const ACCOUNT: &str = send_fixture::ACCOUNT;

/// The second configured account, reachable only by naming it.
pub const OTHER_ACCOUNT: &str = send_fixture::OTHER_ACCOUNT;

/// The configured account with no store on disk: the `-32006` row for every
/// method of this slice that opens one.
pub const STORELESS_ACCOUNT: &str = send_fixture::STORELESS_ACCOUNT;

/// The Microsoft Graph account: `ANO-4` refuses an RSVP for it, exactly as it
/// refuses an invitation.
pub const GRAPH_ACCOUNT: &str = send_fixture::GRAPH_ACCOUNT;

/// The account with an SMTP host, an `[accounts.oauth2]` section it does not
/// use, and `auth_method = "password"`: the `mp config oauth2-login` refusal.
pub const SMTP_ACCOUNT: &str = send_fixture::SMTP_ACCOUNT;

/// An account name no configuration carries.
pub const UNKNOWN_ACCOUNT: &str = send_fixture::UNKNOWN_ACCOUNT;

/// Every configured account, in configuration order, which is the order every
/// all-accounts loop of this slice walks.
pub const ALL_ACCOUNTS: [&str; 5] = send_fixture::ALL_ACCOUNTS;

/// The accounts that have a store on disk.
pub const SEEDED_ACCOUNTS: [&str; 3] = send_fixture::SEEDED_ACCOUNTS;

// ---------------------------------------------------------------------------
// The invitations
// ---------------------------------------------------------------------------

/// The `Message-ID` of the invitation [`ACCOUNT`] may reply to: a
/// `METHOD:REQUEST` with an organizer that is not this account and an attendee
/// that is.
pub const INVITATION: &str = "invitation@example.com";

/// The `Message-ID` of an invitation [`ACCOUNT`] organised, whose attendee
/// replied.
pub const HOSTED: &str = "hosted@example.com";

/// The `Message-ID` of the attendee's `METHOD:REPLY`, the one row
/// `mp calendar rebuild` resolves a status from.
pub const REPLY: &str = "reply@example.com";

/// A message with no `invite.ics` blob at all: the "carries no invitation"
/// refusal.
pub const NOT_AN_INVITATION: &str = read_fixture::BERICHT;

/// The mailbox every seeded message of [`ACCOUNT`] lives in.
pub const MAILBOX: &str = "inbox";

/// The organizer of [`INVITATION`], who receives the RSVP.
pub const ORGANIZER: &str = "chair@tum.de";

/// The address [`ACCOUNT`] replies as, which is its `default_from`.
pub const ATTENDEE: &str = "alpha@example.com";

/// The attendee of [`HOSTED`], whose reply `mp calendar rebuild` folds in.
pub const GUEST: &str = "bob@example.com";

/// The `SUMMARY` of [`INVITATION`], which the RSVP's subject is built from.
pub const INVITATION_SUMMARY: &str = "LOC Day planning";

/// The `UID` of [`INVITATION`].
pub const INVITATION_UID: &str = "admin-uid-1@example.com";

/// The `UID` [`HOSTED`] and [`REPLY`] share, which is what lets the reply
/// resolve against the invitation.
pub const HOSTED_UID: &str = "admin-uid-2@example.com";

/// `ANO-4`, in the sentence `src/main.rs` prints today for an RSVP.
pub const GRAPH_RSVP_REFUSAL: &str =
    "RSVP is not supported for Graph accounts yet (#0036, blocked on #0035)";

/// A selector that names no row.
pub const UNKNOWN_SELECTOR: &str = "nobody@example.com";

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// `<root>/accounts/<account>`.
pub fn account_dir(root: &Path, account: &str) -> PathBuf {
    read_fixture::account_dir(root, account)
}

/// One account's fixture store, opened by the test itself.
pub fn store(root: &Path, account: &str) -> Store {
    read_fixture::store(root, account)
}

/// One account's blob store.
pub fn blobs(root: &Path, account: &str) -> BlobStore {
    BlobStore::new(account_dir(root, account).join("blobs"))
}

/// The contact index cache `mp contacts rebuild` writes and
/// `mp contacts stats` prints the path of.
pub fn contacts_cache(root: &Path, account: &str) -> PathBuf {
    account_dir(root, account).join("contacts-cache.json")
}

/// The encrypted secrets file `mp config reset-secrets` deletes.
///
/// `secrets_backend` is unset in every fixture configuration, so the backend is
/// the default `EncryptedFile` one and the file is `<config_dir>/secrets.enc`;
/// the sandbox points the config directory at the root. No test here touches an
/// OS keyring, deliberately: a keyring write from a test would leave a real
/// entry on the developer's machine.
///
/// [`seed`] does **not** write this file, and could not: its contents are
/// ChaCha20-Poly1305 under a key derived from the machine uid, and every
/// command that opens the backend refuses outright when the file is there and
/// does not decrypt (`Cannot decrypt secrets store at …`). A test that wants a
/// secrets file makes one the only way anything makes one, by storing a
/// password through the shipped `config.set_password`.
pub fn secrets_file(root: &Path) -> PathBuf {
    root.join("secrets.enc")
}

/// The OAuth2 token cache of one account, which `mp config reset-secrets` also
/// deletes.
pub fn token_cache(root: &Path, account: &str) -> PathBuf {
    root.join("tokens").join(format!("{account}.enc"))
}

/// The drafts directory of one account.
pub fn drafts_dir(root: &Path, account: &str) -> PathBuf {
    send_fixture::drafts_dir(root, account)
}

/// The file-era mailstore directory `mp cutover` reports and never deletes.
pub fn legacy_dir(root: &Path, account: &str) -> PathBuf {
    account_dir(root, account).join(LEGACY_MAILBOX)
}

/// The name of the file-era mailbox directory.
pub const LEGACY_MAILBOX: &str = "INBOX";

/// The file-era message inside it.
pub const LEGACY_MESSAGE: &str = "2026-01-02-old-mail.md";

/// The draft with no `id:` field, which `mp cutover` gives one and
/// `mp cutover --dry-run` leaves alone.
pub const LEGACY_DRAFT: &str = "ohne-id.md";

/// That draft's path.
pub fn legacy_draft_path(root: &Path, account: &str) -> PathBuf {
    drafts_dir(root, account).join(LEGACY_DRAFT)
}

/// A received selector for one of this fixture's messages.
pub fn selector(account: &str, message_id: &str) -> String {
    format!("mp://{account}/{MAILBOX}/{message_id}")
}

// ---------------------------------------------------------------------------
// The fake transport
// ---------------------------------------------------------------------------

/// The daemon-side hook that serves the RSVP's SMTP submission and its
/// Sent-mailbox APPEND in process. Set on the daemon, never on a client.
pub const FAKE_TRANSPORT_ENV: &str = send_fixture::FAKE_TRANSPORT_ENV;

/// One event the fake transport recorded.
pub type TransportEvent = send_fixture::TransportEvent;

/// The tree-restoring guard the mutating parity rows take a copy with.
pub type Pristine = send_fixture::Pristine;

/// The hook's value for a transport that accepts everything and files the
/// copy, writing its ledger to `log`.
pub fn fake_transport(log: &Path) -> String {
    send_fixture::fake_transport(log)
}

/// The ledger file the fake transport writes, under a root the test owns.
pub fn transport_log(root: &Path) -> PathBuf {
    send_fixture::transport_log(root)
}

/// Every event the fake transport recorded, in the order it wrote them.
pub fn transport_events(log: &Path) -> Vec<TransportEvent> {
    send_fixture::transport_events(log)
}

// ---------------------------------------------------------------------------
// Seeding
// ---------------------------------------------------------------------------

/// Write the configuration, the stores, the drafts, the outbox, the
/// invitations, the file-era remnants, the secrets and the contact indexes
/// under `root`.
///
/// Call it before starting a daemon against the same root: the daemon loads
/// `config.toml` once, at startup.
pub fn seed(root: &Path) {
    send_fixture::seed(root);
    fs::write(root.join("config.toml"), CONFIG).expect("write config.toml");
    seed_invitations(root);
    seed_legacy_tree(root);
    seed_token_cache(root);
    prebuild_contacts(root);
}

/// The five configured accounts of [`send_fixture`], in the same order, with
/// every port written out. See the module header for why the ports are here.
pub const CONFIG: &str = r#"
[[accounts]]
name = "alpha"
default_from = "alpha@example.com"

[accounts.smtp]
port = 465

[accounts.imap]
port = 993

[accounts.mailboxes.inbox]
server = "INBOX"

[[accounts.mailboxes.extra]]
server = "Team/Reports"

[[accounts]]
name = "beta"
default_from = "beta@example.com"

[accounts.smtp]
port = 465

[accounts.imap]
port = 993

[[accounts]]
name = "delta"
default_from = "delta@example.com"

[accounts.smtp]
port = 465

[accounts.imap]
port = 993

[[accounts]]
name = "graph"
default_from = "graph@example.com"
auth_method = "graph"

[accounts.smtp]
port = 465

[accounts.imap]
port = 993

[accounts.oauth2]
client_id = "11111111-1111-1111-1111-111111111111"
tenant_id = "22222222-2222-2222-2222-222222222222"

[[accounts]]
name = "gamma"
default_from = "gamma@example.com"

[accounts.smtp]
host = "127.0.0.1"
port = 9
username = "gamma@example.com"

[accounts.imap]
port = 993
"#;

/// The three calendar rows: one invitation to answer, one invitation this
/// account organised, and one attendee reply to it.
fn seed_invitations(root: &Path) {
    let mut invitation = read_fixture::email(
        "Chair <chair@tum.de>",
        "alpha@example.com",
        "Invitation: LOC Day planning",
        "Mon, 6 Jul 2026 10:00:00 +0200",
        "please let me know whether you can make it\n",
    );
    invitation.message_id = Some(format!("<{INVITATION}>"));
    invitation.calendar_ics = Some(request_ics(
        INVITATION_UID,
        INVITATION_SUMMARY,
        ORGANIZER,
        ATTENDEE,
    ));
    read_fixture::ingest(root, ACCOUNT, MAILBOX, 5, &invitation);

    let mut hosted = read_fixture::email(
        "Sylvain Hellin <alpha@example.com>",
        "bob@example.com",
        "Invitation: Review",
        "Tue, 7 Jul 2026 10:00:00 +0200",
        "agenda attached\n",
    );
    hosted.message_id = Some(format!("<{HOSTED}>"));
    hosted.flags = MessageFlags::seen(true);
    hosted.calendar_ics = Some(request_ics(HOSTED_UID, "Review", ATTENDEE, GUEST));
    read_fixture::ingest(root, ACCOUNT, MAILBOX, 6, &hosted);

    let mut reply = read_fixture::email(
        "Bob <bob@example.com>",
        "alpha@example.com",
        "Accepted: Review",
        "Wed, 8 Jul 2026 11:00:00 +0200",
        "bob accepted\n",
    );
    reply.message_id = Some(format!("<{REPLY}>"));
    reply.flags = MessageFlags::seen(true);
    reply.calendar_ics = Some(reply_ics(HOSTED_UID, "Review", ATTENDEE, GUEST));
    read_fixture::ingest(root, ACCOUNT, MAILBOX, 7, &reply);
}

/// A `METHOD:REQUEST` an attendee can answer, in the shape
/// `tests/imip_integration.rs` takes from Outlook.
fn request_ics(uid: &str, summary: &str, organizer: &str, attendee: &str) -> Vec<u8> {
    format!(
        "BEGIN:VCALENDAR\r\n\
         PRODID:-//mailypoppins//admin fixture//EN\r\n\
         VERSION:2.0\r\n\
         METHOD:REQUEST\r\n\
         BEGIN:VEVENT\r\n\
         UID:{uid}\r\n\
         SEQUENCE:0\r\n\
         SUMMARY:{summary}\r\n\
         DTSTART:20260720T120000Z\r\n\
         DTEND:20260720T130000Z\r\n\
         LOCATION:Room 4.12\r\n\
         ORGANIZER:mailto:{organizer}\r\n\
         ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{attendee}\r\n\
         END:VEVENT\r\n\
         END:VCALENDAR\r\n"
    )
    .into_bytes()
}

/// The attendee's answer to it.
fn reply_ics(uid: &str, summary: &str, organizer: &str, attendee: &str) -> Vec<u8> {
    format!(
        "BEGIN:VCALENDAR\r\n\
         PRODID:-//mailypoppins//admin fixture//EN\r\n\
         VERSION:2.0\r\n\
         METHOD:REPLY\r\n\
         BEGIN:VEVENT\r\n\
         UID:{uid}\r\n\
         SEQUENCE:0\r\n\
         SUMMARY:{summary}\r\n\
         DTSTART:20260720T120000Z\r\n\
         DTEND:20260720T130000Z\r\n\
         ORGANIZER:mailto:{organizer}\r\n\
         ATTENDEE;PARTSTAT=ACCEPTED:mailto:{attendee}\r\n\
         END:VEVENT\r\n\
         END:VCALENDAR\r\n"
    )
    .into_bytes()
}

/// One file-era mailbox directory and one draft with no `id:` field, which are
/// the two halves `mp cutover` reports.
fn seed_legacy_tree(root: &Path) {
    let dir = legacy_dir(root, ACCOUNT);
    fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    fs::write(dir.join(LEGACY_MESSAGE), LEGACY_MESSAGE_BODY).expect("write the file-era message");

    let drafts = drafts_dir(root, ACCOUNT);
    fs::create_dir_all(&drafts).unwrap_or_else(|e| panic!("create {}: {e}", drafts.display()));
    fs::write(drafts.join(LEGACY_DRAFT), LEGACY_DRAFT_BODY).expect("write the id-less draft");
}

/// A file-era message: front matter and a body, no `id:` anywhere, exactly
/// what the pre-store tree held.
const LEGACY_MESSAGE_BODY: &str = "---\n\
from: someone@example.com\n\
to: alpha@example.com\n\
subject: Old mail\n\
date: 2026-01-02T08:00:00+01:00\n\
---\n\n\
this one predates the store\n";

/// A draft with no `id:` field: the one thing `mp cutover` writes.
const LEGACY_DRAFT_BODY: &str = "---\n\
to: bob@example.com\n\
subject: Ohne id\n\
status: draft\n\
---\n\n\
kein id-Feld\n";

/// One OAuth2 token cache, so `mp config reset-secrets` has something to
/// remove and the removal is a fact on disk rather than a sentence.
///
/// Opaque bytes, because nothing in this slice reads a token's *contents*:
/// `reset-secrets` unlinks the file and no row here authenticates. The secrets
/// file is deliberately absent - see [`secrets_file`] - so the accounts a test
/// runs against have no stored password, which is also true of every other
/// Phase 4 fixture.
fn seed_token_cache(root: &Path) {
    let tokens = root.join("tokens");
    fs::create_dir_all(&tokens).unwrap_or_else(|e| panic!("create {}: {e}", tokens.display()));
    fs::write(token_cache(root, GRAPH_ACCOUNT), b"opaque-token").expect("write the token cache");
}

/// Build every seeded account's contact index once, with the oracle, so
/// `built_at` is one value for both binaries and no parity row builds an index
/// as a side effect.
fn prebuild_contacts(root: &Path) {
    for account in SEEDED_ACCOUNTS {
        let out = oracle_command(root)
            .args(["contacts", "rebuild", "--account", account])
            .output()
            .unwrap_or_else(|e| panic!("prebuild the contact index of {account}: {e}"));
        assert!(
            out.status.success(),
            "prebuilding the contact index of {account} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            contacts_cache(root, account).is_file(),
            "the prebuild wrote no cache at {}",
            contacts_cache(root, account).display()
        );
    }
}

// ---------------------------------------------------------------------------
// Reading the state back
// ---------------------------------------------------------------------------

/// How many contacts one account's cache holds, without the `built_at` the
/// rebuild moves on every run.
///
/// The count is the fact a rebuild is judged on; the timestamp is the one
/// value in the file that two runs cannot agree about, so it is read past
/// rather than masked in a byte comparison.
pub fn cached_contacts(root: &Path, account: &str) -> usize {
    let path = contacts_cache(root, account);
    let Ok(text) = fs::read_to_string(&path) else {
        return 0;
    };
    let value: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    value["contacts"]
        .as_object()
        .map(|map| map.len())
        .or_else(|| value["contacts"].as_array().map(|rows| rows.len()))
        .unwrap_or_else(|| panic!("the contact cache carries a `contacts` collection: {value}"))
}

/// Every address one account's cache holds, sorted: a rebuild that found the
/// same rows in a different order is the same rebuild.
pub fn cached_addresses(root: &Path, account: &str) -> Vec<String> {
    let path = contacts_cache(root, account);
    let Ok(text) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let value: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    let mut out: Vec<String> = match &value["contacts"] {
        serde_json::Value::Object(map) => map.keys().cloned().collect(),
        serde_json::Value::Array(rows) => rows
            .iter()
            .filter_map(|row| row["address"].as_str().map(str::to_string))
            .collect(),
        other => panic!("the contact cache carries a `contacts` collection: {other}"),
    };
    out.sort();
    out
}

/// Whether the secrets file and the token cache `mp config reset-secrets`
/// removes are there. The first is `false` until something stores a password.
pub fn secrets_present(root: &Path) -> (bool, bool) {
    (
        secrets_file(root).is_file(),
        token_cache(root, GRAPH_ACCOUNT).is_file(),
    )
}

/// The bytes of the id-less draft, so a `--dry-run` that wrote an `id:` field
/// is caught.
pub fn legacy_draft_bytes(root: &Path, account: &str) -> Vec<u8> {
    fs::read(legacy_draft_path(root, account)).unwrap_or_default()
}

/// The id-less draft after a cutover, with the minted id replaced by a marker.
///
/// The `id:` a cutover assigns is minted per run (`drafts_index`), so two
/// binaries importing the same draft write two different ids into the same
/// file. What has to agree is that the field is there and that nothing else
/// moved, which is what this projection compares.
pub fn legacy_draft_shape(root: &Path, account: &str) -> String {
    String::from_utf8_lossy(&legacy_draft_bytes(root, account))
        .lines()
        .map(|line| {
            if line.starts_with("id:") {
                "id: <minted>".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether the file-era mailstore is still on disk. `mp cutover` deletes
/// nothing, by design (`MIG-01`), so this is `true` after every row.
pub fn legacy_tree_present(root: &Path, account: &str) -> bool {
    legacy_dir(root, account).join(LEGACY_MESSAGE).is_file()
}

/// Every blob file one account holds, sorted: what a sweep evicted, and what a
/// `--dry-run` left alone.
pub fn blob_files(root: &Path, account: &str) -> Vec<String> {
    let mut out = Vec::new();
    let dir = account_dir(root, account).join("blobs");
    collect_files(&dir, &dir, &mut out);
    out.sort();
    out
}

fn collect_files(base: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(base, &path, out);
        } else if let Ok(rest) = path.strip_prefix(base) {
            out.push(rest.to_string_lossy().into_owned());
        }
    }
}

/// A sandboxed invocation of the oracle, for a fixture step that has to run a
/// command rather than call a library function.
pub fn oracle_run(root: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd: Command = oracle_command(root);
    cmd.args(args)
        .output()
        .unwrap_or_else(|e| panic!("run oracle `mp {}`: {e}", args.join(" ")))
}

/// A message with nothing special about it, for a test that needs one more row.
pub fn plain_email(from: &str, to: &str, subject: &str, date: &str, body: &str) -> FetchedEmail {
    read_fixture::email(from, to, subject, date, body)
}
