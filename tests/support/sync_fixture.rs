//! The seeded root the Phase 4 sync/watch slice is measured against (plan
//! P4-U9).
//!
//! `mp sync`, `mp fetch`, `mp list-mailboxes` and `mp watch` all need a
//! *server*, and this fixture has none. That is not a gap to apologise for: it
//! is what makes the slice testable at all. Every one of those commands walks a
//! fixed prefix before it opens a socket - resolve the account, resolve its
//! sync targets, resolve its credentials - and that prefix is where all of the
//! deterministic behaviour lives. What the fixture pins is therefore every way
//! the four commands *refuse*, plus the one success `mp sync` can have without
//! a server: an account that has nothing to sync.
//!
//! # The four accounts, and what each one is for
//!
//! [`read_fixture`]'s three, unchanged so the read slice's expectations still
//! hold, plus one of this slice's own:
//!
//! | account | shape | what it exercises |
//! |---|---|---|
//! | `alpha` | no `[accounts.imap]`, no `[accounts.smtp]` | [`AccountConfig::is_local_only`]: `mp sync` skips it and exits 0 |
//! | `beta` | the same | the second skipped account of `--all-accounts` |
//! | `delta` | the same, and no store on disk | the third |
//! | `gamma` | an IMAP host, two mailboxes, no credentials | every refusal that needs a *configured* server |
//!
//! `gamma` is the whole reason this fixture is not just `read_fixture`. A
//! local-only account never reaches target resolution or credential
//! resolution, so without it the unknown-mailbox refusal and the missing-secret
//! refusal are both unreachable and `mp sync` has exactly one row.
//!
//! `gamma`'s IMAP host is `127.0.0.1:9`, the discard port, and **nothing ever
//! connects to it**: credential resolution fails first, in both binaries, on
//! every command of this slice. The address is there so the account is not
//! local-only, not so that a connection is attempted, and a test that ever saw
//! a connection error rather than [`secret_refusal`] would be reporting a real
//! ordering change.
//!
//! # No fake IMAP server, and what that costs
//!
//! `rg -l 'fake_imap|FakeImap|MockImap|imap_server' tests src crates` finds
//! nothing: this repository has never had an in-process IMAP server, and the
//! nearest thing to one is `tests/engine_lock_ingest_cli.rs`'s sentinel
//! `TcpListener`, which exists to prove that *no* connection was made.
//!
//! So the successful half of the slice - a sync that ingests, a mailbox list
//! that comes back from a server, an IDLE that fires - is pinned at the wire
//! level as far as a fixture with no server can reach it, and no further:
//! the operation id is issued, `operation.status` answers about it, and the
//! operation settles `failed` carrying the refusal. Building the fake server
//! that would unlock the rest is a follow-up, recorded in the test file's
//! header rather than invented here.
//!
//! # The engine lock
//!
//! [`seed_secrets`] writes `gamma`'s two passwords into the sandbox's own
//! encrypted secrets file, which moves the refusal one step later: past
//! credential resolution and into the guarded sync itself. That is exactly
//! enough to reach the engine-lock refusal
//! (`tests/engine_lock_ingest_cli.rs`), which is the one *success* path of
//! `mp sync` that needs no server at all - the run that does nothing because
//! another process is this account's engine.
//!
//! It returns `false` on a host with no readable machine ID, where
//! [`EncryptedFileBackend`] cannot open, and the caller skips rather than
//! fails: `tests/secrets_integration.rs` and `tests/engine_lock_ingest_cli.rs`
//! both already do this.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use mailypoppins::secrets::{EncryptedFileBackend, SecretsBackend};
use mailypoppins::store::Store;

use super::read_fixture;

/// The default account: configured, local-only, and the one every `-A`-less
/// command of this slice answers about.
pub const ACCOUNT: &str = read_fixture::ACCOUNT;

/// The second local-only account.
pub const OTHER_ACCOUNT: &str = read_fixture::OTHER_ACCOUNT;

/// The third local-only account, which additionally has no store.
pub const STORELESS_ACCOUNT: &str = read_fixture::STORELESS_ACCOUNT;

/// The account with a server configured and no credentials for it.
pub const SERVER_ACCOUNT: &str = "gamma";

/// An account name no configuration carries.
pub const UNKNOWN_ACCOUNT: &str = read_fixture::UNKNOWN_ACCOUNT;

/// Every configured account, in configuration order, which is the order
/// `mp sync --all-accounts` walks them in and prints their headers.
pub const ALL_ACCOUNTS: [&str; 4] = [ACCOUNT, OTHER_ACCOUNT, STORELESS_ACCOUNT, SERVER_ACCOUNT];

/// The three accounts `mp sync` skips because they have nothing to sync.
pub const LOCAL_ONLY_ACCOUNTS: [&str; 3] = [ACCOUNT, OTHER_ACCOUNT, STORELESS_ACCOUNT];

/// The server-name of [`SERVER_ACCOUNT`]'s inbox, and the only value
/// `--mailbox` accepts beside `Team/Reports`.
pub const INBOX: &str = "INBOX";

/// The second configured mailbox of [`SERVER_ACCOUNT`], whose id is neither a
/// role nor lowercase.
pub const EXTRA_MAILBOX: &str = "Team/Reports";

/// A mailbox name no account configures.
pub const UNKNOWN_MAILBOX: &str = "nosuch";

/// The discard port. Configured so [`SERVER_ACCOUNT`] is not local-only, never
/// connected to, because credential resolution refuses first.
pub const DISCARD_PORT: u16 = 9;

/// The configuration this fixture writes: [`read_fixture::CONFIG`] with
/// [`SERVER_ACCOUNT`] appended, so `alpha` is still first and still the
/// default account and the read slice's account set is untouched.
pub const GAMMA: &str = r#"
[[accounts]]
name = "gamma"
default_from = "gamma@example.com"

[accounts.imap]
host = "127.0.0.1"
port = 9
username = "gamma@example.com"

[accounts.mailboxes.inbox]
server = "INBOX"

[[accounts.mailboxes.extra]]
server = "Team/Reports"
"#;

/// `<root>/accounts/<account>`.
pub fn account_dir(root: &Path, account: &str) -> PathBuf {
    read_fixture::account_dir(root, account)
}

/// `<root>/accounts/<account>/store.lock`, the file an engine holds.
pub fn engine_lock_path(root: &Path, account: &str) -> PathBuf {
    account_dir(root, account).join("store.lock")
}

/// Write the configuration, the stores and [`SERVER_ACCOUNT`]'s account
/// directory under `root`.
///
/// Call it before starting a daemon against the same root: the daemon loads
/// `config.toml` once, at startup.
pub fn seed(root: &Path) {
    read_fixture::seed(root);
    fs::write(
        root.join("config.toml"),
        format!("{}{GAMMA}", read_fixture::CONFIG),
    )
    .expect("write config.toml");

    // A store the guarded sync can lock, so a refusal is about the lock rather
    // than about an account directory that is not there.
    let dir = account_dir(root, SERVER_ACCOUNT);
    fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    drop(Store::open(dir.join("store.sqlite3")).expect("open the gamma store"));
}

/// Seed [`SERVER_ACCOUNT`]'s IMAP and SMTP passwords into the sandbox's own
/// encrypted secrets file.
///
/// `false` means this host has no readable machine ID and the backend cannot
/// open, which is a reason to skip a test rather than fail it.
pub fn seed_secrets(root: &Path) -> bool {
    let Ok(secrets) = EncryptedFileBackend::open(root.join("secrets.enc")) else {
        return false;
    };
    for key in [
        format!("imap-password-{SERVER_ACCOUNT}"),
        format!("smtp-password-{SERVER_ACCOUNT}"),
    ] {
        if secrets.set(&key, "hunter2").is_err() {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// The refusals, verbatim
// ---------------------------------------------------------------------------

/// The sentence every command of this slice refuses an account with no
/// credentials with, from the secret store itself.
///
/// It names the *SMTP* key even on a purely IMAP path, because `ImapConfig`
/// falls back to the SMTP password; that is today's behaviour and parity means
/// reproducing it rather than improving it.
pub fn secret_refusal(account: &str) -> String {
    format!("Secret 'smtp-password-{account}' not found. Run `mp config set-password`.")
}

/// The line `mp sync` prints for an account that has nothing to sync.
pub fn local_only_line(account: &str) -> String {
    format!("- {account}: local-only, skipped")
}

/// `mp sync --all-accounts`'s per-account header.
pub fn account_header(account: &str) -> String {
    format!("── {account} ──")
}

/// The refusal a `--mailbox` no account configures earns, before anything
/// opens a socket.
pub fn unknown_mailbox_refusal(account: &str) -> String {
    format!(
        "account '{account}' has no mailbox '{UNKNOWN_MAILBOX}' configured; \
         it knows {INBOX}, {EXTRA_MAILBOX}"
    )
}

/// The failure summary `mp sync` prints when the one account it attempted
/// failed. Skipped accounts are out of the denominator.
pub fn failure_summary(failed: &str) -> String {
    format!("1 of 1 account(s) failed to sync: {failed}")
}

/// `-A` naming an account the configuration does not carry leaves nothing to
/// sync, and the command says so rather than syncing the default.
pub const NO_ACCOUNT_TO_SYNC: &str = "No account to sync (check `mp config show`)";

/// `mp watch` on a Graph account, which has no IDLE to offer.
pub const GRAPH_WATCH_REFUSAL: &str =
    "IMAP IDLE watch is not supported for Graph accounts. Use 'mp sync' instead.";

/// The line `mp sync` prints when another process is this account's engine.
///
/// Pinned by `tests/engine_lock_ingest_cli.rs` (#0122); repeated here because
/// the routed twin has to reproduce it byte for byte.
pub fn engine_busy_line(account: &str) -> String {
    format!("Sync skipped: another engine is syncing '{account}'; leaving the ingest to it")
}
