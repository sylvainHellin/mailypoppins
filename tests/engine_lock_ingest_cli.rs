//! `mp sync` refusing the engine lock, in a binary of its own (#0122).
//!
//! This is one test, split out of `tests/engine_lock_ingest.rs` because it is
//! the only one in the phase that spawns a process, and a spawned process is
//! what made that binary flake about twice in twenty-five runs.
//!
//! `Command::spawn` forks, and the child inherits every open file descriptor
//! until `exec` closes the `O_CLOEXEC` ones. `flock(2)` is scoped to the open
//! file description, so for that window the *inherited copies* of the
//! descriptors a sibling test thread holds keep that sibling's lock alive even
//! after the sibling drops its own `EngineLock`. The sibling then sees its own
//! tempdir lock refused by nothing, and a guarded pass returns `Ok(None)` where
//! the test expects a real [`mailypoppins::sync::SyncResult`].
//!
//! Nothing in this binary holds an engine lock on a thread that a fork could
//! race, because the only lock here is the one this test deliberately hands to
//! the child to be refused, and there is no sibling test to inherit it.
//!
//! The engine lock on the ingest path is a change to library behaviour that
//! every build ships, so this runs in the default suite exactly as
//! `tests/engine_lock_ingest.rs` does. It had an explicit `[[test]]` target in
//! `Cargo.toml` while the daemon feature existed, to keep the split visible
//! beside the gated targets; P4-U1 removed every stanza and Cargo autodiscovers
//! `tests/*.rs`, so the split now lives only in this header.
//!
//! Since P4-U10 the `mp sync` this test spawns is a daemon client, so the child
//! it forks starts a daemon of its own against the sandbox. The temporary tree
//! is therefore a [`support::parity::SandboxRoot`], which stops that daemon when
//! the tree goes: the assertions are untouched, and what changes is only that
//! the suite no longer leaves a daemon holding a deleted directory (and, with
//! it, an inherited copy of this test's engine lock).

mod support;

use std::fs;
use std::net::TcpListener;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use mailypoppins::engine_lock::EngineLock;
use mailypoppins::secrets::{EncryptedFileBackend, SecretsBackend};
use mailypoppins::store::Store;

const MP: &str = env!("CARGO_BIN_EXE_mp");

/// The line `mp sync` prints when another process holds the engine lock.
///
/// Pinned here because the plan leaves the wording to this unit. It mirrors
/// what the outbox already says on the same refusal (`[outbox] another engine
/// is draining {account}; leaving the APPENDs to it`), moved from the log to
/// stdout because `mp sync` is an interactive command whose whole output is
/// otherwise a summary of a pass that did not happen. The full line carries the
/// `ℹ` prefix every other informational `mp sync` line uses:
///
/// ```text
/// ℹ Sync skipped: another engine is syncing 'acct'; leaving the ingest to it
/// ```
///
/// The test asserts on the text after the prefix, so the marker and any
/// colouring stay the implementer's business.
fn skip_line(account: &str) -> String {
    format!("Sync skipped: another engine is syncing '{account}'; leaving the ingest to it")
}

/// `mp sync` treats the refusal as a success: exit 0, one line saying the sync
/// was skipped and why, and no summary claiming a pass that never ran.
///
/// The sandbox account points IMAP at a listener this test owns, and the test
/// asserts afterwards that nothing ever connected to it: that is the "no IMAP
/// session" half of the contract, asserted at the CLI rather than through a
/// fake backend. The password is seeded into the sandbox's own encrypted
/// secrets file so a run that does connect gets as far as the socket rather
/// than dying on a missing credential, which leaves the guard's *placement*
/// free (before or after `ImapConfig::load`) and pins only its effect.
///
/// Without the guard this command exits 0 too: a per-target fetch failure is a
/// warning, so today's run prints `✓ Synced: 0 email(s) ingested` after a
/// refused connection. The skip line and the accepted-connection count are what
/// make the test non-vacuous.
#[test]
fn mp_sync_exits_zero_and_says_it_skipped_when_another_process_holds_the_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let config_dir = tmp.path().join("config");
    let data_dir = tmp.path().join("data");
    // Owns the tree from here, and stops the daemon the spawned `mp sync`
    // starts. The paths above are unchanged: the guard only decides when the
    // tree and the daemon go away.
    let tmp = support::parity::SandboxRoot::new(tmp, &data_dir);
    let account_dir = data_dir.join("accounts").join("acct");
    fs::create_dir_all(&config_dir).unwrap();
    fs::create_dir_all(&account_dir).unwrap();

    // The default backend is the machine-keyed encrypted file under the config
    // directory, which the sandbox owns. A host with no readable machine ID
    // cannot seed it; skip there rather than fail, as
    // `tests/secrets_integration.rs` does.
    let Ok(secrets) = EncryptedFileBackend::open(config_dir.join("secrets.enc")) else {
        return;
    };
    secrets.set("imap-password-acct", "hunter2").unwrap();
    secrets.set("smtp-password-acct", "hunter2").unwrap();

    // A listener the test owns rather than a dead port, so "nothing connected"
    // is a fact this test can check instead of infer. It closes every
    // connection immediately: a client that does connect fails its TLS
    // handshake at once instead of blocking the suite on a server that never
    // answers.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the sentinel IMAP port");
    let port = listener.local_addr().unwrap().port();
    let connections = Arc::new(AtomicUsize::new(0));
    {
        let seen = Arc::clone(&connections);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        seen.fetch_add(1, Ordering::SeqCst);
                        drop(stream);
                    }
                    Err(_) => break,
                }
            }
        });
    }

    fs::write(
        config_dir.join("config.toml"),
        format!(
            r#"
[[accounts]]
name = "acct"
default_from = "acct@example.com"

[accounts.imap]
host = "127.0.0.1"
port = {port}
username = "acct@example.com"

[accounts.mailboxes.inbox]
server = "INBOX"
"#
        ),
    )
    .unwrap();
    // A store the sync would have written into, so the run fails on the lock
    // rather than on an absent account directory.
    drop(Store::open(account_dir.join("store.sqlite3")).unwrap());

    let _holder = EngineLock::try_acquire_at(&account_dir.join("store.lock"), "acct")
        .unwrap()
        .expect("the test process holds the lock the child will be refused");

    let out = Command::new(MP)
        .arg("sync")
        .env("HOME", tmp.path())
        .env("MAILYPOPPINS_CONFIG_DIR", &config_dir)
        .env("MAILYPOPPINS_DATA_DIR", &data_dir)
        .env("NO_COLOR", "1")
        .output()
        .expect("mp must run");

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    assert_eq!(
        out.status.code(),
        Some(0),
        "a refused sync is a success, not a failure\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains(&skip_line("acct")),
        "`mp sync` must say why it did nothing; expected {:?}\nstdout: {stdout}",
        skip_line("acct")
    );
    assert!(
        !stdout.contains("Synced:"),
        "a skipped sync must not report a pass it never ran\nstdout: {stdout}"
    );

    assert_eq!(
        connections.load(Ordering::SeqCst),
        0,
        "a refused sync must open no IMAP session, but the sentinel port was connected to"
    );
}
