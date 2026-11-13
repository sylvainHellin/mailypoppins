//! The secrets smoke test (PERSO-199): `mp config init`, then
//! `mp config set-password`, then the SMTP and IMAP configuration reading the
//! stored password back, all through the real binary and a real daemon over a
//! temporary config and data root, once per secrets backend.
//!
//! # What stands in for the mail servers
//!
//! The wizard dials the account it is describing before it writes anything
//! (`config.probe`), and the read-back is a login, so both servers are fakes on
//! localhost: [`RecordingSmtpServer`] and [`RejectingImapServer`] in its
//! accepting mode. Each records the password a login carried, which is what
//! proves the password a client used is the one stored, not merely that some
//! password was found. Both speak STARTTLS with a self-signed identity, so the
//! wizard is answered with the Proton Bridge preset, the one that writes
//! `accept_invalid_certs = true` for a loopback host.
//!
//! # How the wizard is answered
//!
//! Through a pipe: every prompt reads a line of stdin, and the password prompt
//! reads one too when stdin is not a terminal. The one prompt a pipe cannot
//! answer is a mailbox picker (`dialoguer::FuzzySelect` needs a terminal), so
//! the fake IMAP server lists no folder while the wizard runs, and the wizard
//! asks for every mailbox name as a line instead.
//!
//! # The read-back
//!
//! - IMAP: `mp sync` makes the daemon build the account's `ImapConfig` from
//!   the store and log in; the fake records the password.
//! - SMTP: every non-TUI command loads the first account's `SmtpConfig` in the
//!   client's own process and warns `Could not load SMTP config` when it
//!   cannot (`src/main.rs`), so a command with no such line read the password
//!   back; the encrypted-file run also decrypts the file and compares values.
//!
//! # The backends
//!
//! The encrypted file runs everywhere. The keyring runs where it needs no
//! desktop session: on Linux the `linux-native` backend is the kernel keyring,
//! which the test process, the daemon and every client share through the
//! session keyring they inherit. On macOS it is the login Keychain, which a
//! test must not write by default, so that test is ignored there and run with
//! `cargo test --test secrets_smoke -- --ignored` on the Mac.

mod support;

use std::io::Write;
use std::path::Path;
use std::process::{Output, Stdio};
use std::time::Instant;

use support::fake_imap::{FakeFolder, LoginPolicy, RejectingImapServer};
use support::fake_smtp::{RecordingSmtpServer, SmtpLogin};
use support::parity::{mp_command, DaemonFixture, DEADLINE, TICK};

/// The address the account logs in and sends as.
const USERNAME: &str = "smoke@example.com";

/// The passwords the wizard stores.
const SMTP_AT_INIT: &str = "smtp init \"secret\"";
const IMAP_AT_INIT: &str = "imap-init-secret";

/// The passwords `mp config set-password` replaces them with.
const SMTP_SET: &str = "smtp-set-secret";
const IMAP_SET: &str = "imap set \\secret";

/// A temporary root under the cargo-configured `TMPDIR` (`/var/tmp`).
fn root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("mp-secrets-smoke-")
        .tempdir()
        .expect("a temporary root")
}

/// Run `mp` against `root` with `input` on its stdin.
fn mp_with_stdin(root: &Path, args: &[&str], input: &str) -> Output {
    let mut child = mp_command(root)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn `mp {}`: {e}", args.join(" ")));
    child
        .stdin
        .take()
        .expect("a piped stdin")
        .write_all(input.as_bytes())
        .expect("write the answers");
    child
        .wait_with_output()
        .unwrap_or_else(|e| panic!("run `mp {}`: {e}", args.join(" ")))
}

fn mp(root: &Path, args: &[&str]) -> Output {
    mp_command(root)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("run `mp {}`: {e}", args.join(" ")))
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn assert_success(out: &Output, what: &str) {
    assert!(
        out.status.success(),
        "{what} failed ({:?})\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        stdout(out),
        stderr(out)
    );
}

/// Every answer of `mp config init` for a Proton Bridge account on the two
/// fakes, one line per prompt, in the order the wizard asks.
fn wizard_answers(account: &str, smtp_port: u16, imap_port: u16) -> String {
    [
        "2",                    // provider: Proton Mail (via Proton Bridge)
        account,                // account name
        "",                     // SMTP host: the preset's 127.0.0.1
        &smtp_port.to_string(), // SMTP port
        USERNAME,               // SMTP username
        "",                     // default from: the username
        SMTP_AT_INIT,           // SMTP password, probed
        "",                     // IMAP host: the preset's 127.0.0.1
        &imap_port.to_string(), // IMAP port
        "",                     // IMAP username: the SMTP one
        "n",                    // same password as SMTP? no
        IMAP_AT_INIT,           // IMAP password, probed
        "",                     // inbox name: INBOX
        "",                     // archive name: Archive
        "",                     // sent name: Sent
        "",                     // drafts: the one the server marks
        "",                     // trash: the one the server marks
        "",                     // junk: the one the server marks
        "",                     // overrides: none
    ]
    .map(|line| format!("{line}\n"))
    .concat()
}

/// The two fake servers, the IMAP one listing nothing while the wizard runs.
struct Servers {
    smtp: RecordingSmtpServer,
    imap: RejectingImapServer,
}

impl Servers {
    fn start() -> Servers {
        let imap = RejectingImapServer::start_with(LoginPolicy::Accept);
        imap.set_folders(Vec::new());
        Servers {
            smtp: RecordingSmtpServer::start(),
            imap,
        }
    }
}

/// `mp config init`, answered through the pipe, against a daemon with no
/// configuration yet: both probes log in with the typed passwords and both
/// passwords are stored.
fn run_wizard(root: &Path, servers: &Servers, account: &str) {
    let out = mp_with_stdin(
        root,
        &["config", "init"],
        &wizard_answers(account, servers.smtp.port(), servers.imap.port()),
    );
    assert_success(&out, "mp config init");
    let printed = stdout(&out);
    assert!(
        printed.contains("SMTP password stored") && printed.contains("IMAP password stored"),
        "the wizard stores both passwords:\n{printed}"
    );
    assert_eq!(
        servers.smtp.logins(),
        vec![SmtpLogin {
            username: USERNAME.to_string(),
            password: SMTP_AT_INIT.to_string(),
        }],
        "the SMTP probe logged in with the typed password"
    );
    assert!(
        servers.imap.passwords().iter().any(|p| p == IMAP_AT_INIT),
        "the IMAP probe logged in with the typed password: {:?}",
        servers.imap.passwords()
    );
}

/// `mp config set-password <kind> -A <account> --stdin`.
fn set_password(root: &Path, kind: &str, account: &str, value: &str) {
    let out = mp_with_stdin(
        root,
        &["config", "set-password", kind, "-A", account, "--stdin"],
        &format!("{value}\n"),
    );
    assert_success(&out, &format!("mp config set-password {kind}"));
    assert!(
        stdout(&out).contains(&format!(
            "{} password for '{account}' stored",
            kind.to_uppercase()
        )),
        "the stored line:\n{}",
        stdout(&out)
    );
}

/// The read-back through both configurations.
///
/// IMAP: the daemon logs in with the password the store holds now. The
/// account's first sync, which `config.init` starts before the wizard has
/// stored the passwords, may still hold a session, and so may the watcher,
/// so the fake cuts every session before each `mp sync` until a login after
/// the last `set-password` arrives. SMTP: the client's own `SmtpConfig::load`
/// for the first account, which every non-TUI command runs and which warns
/// when it fails.
fn assert_read_back(root: &Path, servers: &Servers, account: &str) {
    servers.imap.set_folders(vec![FakeFolder::empty("INBOX")]);
    let before = servers.imap.passwords().len();
    let start = Instant::now();
    loop {
        servers.imap.drop_sessions();
        let out = mp(root, &["sync", "-A", account]);
        assert_success(&out, "mp sync");
        if servers.imap.passwords()[before..]
            .iter()
            .any(|p| p == IMAP_SET)
        {
            break;
        }
        assert!(
            start.elapsed() < DEADLINE,
            "no IMAP login used the password set last within {DEADLINE:?}: {:?}",
            &servers.imap.passwords()[before..]
        );
        std::thread::sleep(TICK);
    }

    let out = mp(root, &["config", "show"]);
    assert_success(&out, "mp config show");
    assert!(
        !stderr(&out).contains("Could not load SMTP config"),
        "the client loaded the SMTP configuration with its password:\n{}",
        stderr(&out)
    );
}

/// The encrypted-file backend, the default: the wizard writes the file, the
/// passwords set afterwards replace the wizard's, and both configurations
/// read the replacements back.
#[test]
fn init_then_set_password_then_load_on_the_encrypted_file_backend() {
    let servers = Servers::start();
    let tmp = root();
    let daemon = DaemonFixture::start(tmp.path());
    let account = "smoke";

    run_wizard(daemon.root(), &servers, account);
    let secrets = daemon.root().join("secrets.enc");
    assert!(secrets.is_file(), "the wizard wrote {}", secrets.display());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&secrets).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the secrets file is the owner's alone");
    }

    set_password(daemon.root(), "smtp", account, SMTP_SET);
    set_password(daemon.root(), "imap", account, IMAP_SET);

    use mailypoppins::secrets::{EncryptedFileBackend, SecretsBackend};
    let file = EncryptedFileBackend::open(secrets).expect("this machine decrypts its own file");
    assert_eq!(
        file.get(&format!("smtp-password-{account}")).unwrap(),
        SMTP_SET
    );
    assert_eq!(
        file.get(&format!("imap-password-{account}")).unwrap(),
        IMAP_SET
    );

    assert_read_back(daemon.root(), &servers, account);
    daemon.stop();
}

/// The keyring backend. The wizard has no backend question, so it writes the
/// default encrypted file; the configuration then names the keyring, the
/// daemon restarts (the backend is chosen once per process), and the
/// passwords set afterwards go to the keyring and are read back from it.
///
/// The account name carries the process id so a keyring entry can never be
/// one of a real account's, and the entries are deleted on the way out.
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "writes to the login Keychain; run on the Mac with --ignored"
)]
fn init_then_set_password_then_load_on_the_keyring_backend() {
    let servers = Servers::start();
    let tmp = root();
    let account = format!("smoke{}", std::process::id());
    let cleanup = KeyringCleanup(account.clone());

    let daemon = DaemonFixture::start(tmp.path());
    run_wizard(daemon.root(), &servers, &account);
    daemon.stop();

    let config_path = tmp.path().join("config.toml");
    let config = std::fs::read_to_string(&config_path).expect("the wizard wrote config.toml");
    std::fs::write(
        &config_path,
        format!("secrets_backend = \"keyring\"\n{config}"),
    )
    .expect("name the keyring backend");
    // The encrypted file the wizard wrote goes, so a read that still reached
    // it could not find the passwords.
    std::fs::remove_file(tmp.path().join("secrets.enc")).expect("remove the wizard's file");

    let daemon = DaemonFixture::start(tmp.path());
    set_password(daemon.root(), "smtp", &account, SMTP_SET);
    set_password(daemon.root(), "imap", &account, IMAP_SET);
    assert!(
        !tmp.path().join("secrets.enc").exists(),
        "the keyring backend writes no secrets file"
    );

    assert_read_back(daemon.root(), &servers, &account);
    daemon.stop();

    // The kernel keyring is this process's too, so the values are read here
    // directly; on the Mac a read from this binary would ask for Keychain
    // access to an item `mp` created, so it is left to the logins above.
    #[cfg(target_os = "linux")]
    for (kind, value) in [("smtp", SMTP_SET), ("imap", IMAP_SET)] {
        assert_eq!(
            keyring_entry(kind, &account).get_password().unwrap(),
            value,
            "the keyring holds the {kind} password set last"
        );
    }
    drop(cleanup);
    for kind in ["smtp", "imap"] {
        assert!(
            matches!(
                keyring_entry(kind, &account).get_password(),
                Err(keyring::Error::NoEntry)
            ),
            "the test leaves no {kind} entry behind"
        );
    }
}

/// The keyring entry `mp` files a password of `account` under on the default
/// profile.
fn keyring_entry(kind: &str, account: &str) -> keyring::Entry {
    keyring::Entry::new("mailypoppins", &format!("{kind}-password-{account}"))
        .expect("a keyring entry")
}

/// Delete the keyring entries of a test account, whatever the test did.
struct KeyringCleanup(String);

impl Drop for KeyringCleanup {
    fn drop(&mut self) {
        for kind in ["smtp", "imap"] {
            let _ = keyring_entry(kind, &self.0).delete_credential();
        }
    }
}

// ---------------------------------------------------------------------------
// The errors a user can meet on that path
// ---------------------------------------------------------------------------

/// One password account on servers nobody listens on, with no scheduled sync,
/// written directly so a test starts from a configuration and no password.
fn write_config(root: &Path, account: &str) {
    std::fs::write(
        root.join("config.toml"),
        format!(
            "[[accounts]]\nname = \"{account}\"\ndefault_from = \"{USERNAME}\"\n\n\
             [accounts.smtp]\nhost = \"127.0.0.1\"\nport = 9\nusername = \"{USERNAME}\"\n\n\
             [accounts.imap]\nport = 9\nsync_interval_secs = 0\n"
        ),
    )
    .expect("write config.toml");
}

/// A secrets file this machine cannot decrypt (copied from another machine,
/// or another user's) is named with the command that recovers from it, and
/// that command runs: the client used to refuse it with the very sentence
/// that recommends it.
#[test]
fn an_undecryptable_secrets_file_is_named_and_mp_config_reset_secrets_recovers_it() {
    let tmp = root();
    write_config(tmp.path(), "smoke");
    let secrets = tmp.path().join("secrets.enc");
    // The header of a version-1 file, a zero nonce and a ciphertext no key of
    // this machine authenticates.
    let mut foreign = b"MPSEC\x01".to_vec();
    foreign.extend([0u8; 12]);
    foreign.extend([0x5au8; 48]);
    std::fs::write(&secrets, &foreign).expect("write a foreign secrets file");
    let daemon = DaemonFixture::start(tmp.path());

    let out = mp_with_stdin(
        daemon.root(),
        &["config", "set-password", "smtp", "-A", "smoke", "--stdin"],
        &format!("{SMTP_SET}\n"),
    );
    assert!(
        !out.status.success(),
        "nothing is stored over a foreign file"
    );
    let said = stderr(&out);
    assert!(
        said.contains(&format!(
            "Cannot decrypt secrets store at {}",
            secrets.display()
        )) && said.contains("different machine or by a different user")
            && said.contains("Run `mp config reset-secrets`"),
        "what happened and what to do next:\n{said}"
    );

    // The password prompts that follow the reset need a terminal, so only the
    // reset itself is asserted here.
    let out = mp_with_stdin(daemon.root(), &["config", "reset-secrets"], "y\n");
    assert!(
        stdout(&out).contains(&format!("Removed {}", secrets.display())),
        "mp config reset-secrets runs over a foreign file:\nstdout:\n{}\nstderr:\n{}",
        stdout(&out),
        stderr(&out)
    );
    assert!(!secrets.exists(), "the foreign file is gone");

    set_password(daemon.root(), "smtp", "smoke", SMTP_SET);
    use mailypoppins::secrets::{EncryptedFileBackend, SecretsBackend};
    let file = EncryptedFileBackend::open(secrets).expect("the new file is this machine's");
    assert_eq!(file.get("smtp-password-smoke").unwrap(), SMTP_SET);
    daemon.stop();
}
