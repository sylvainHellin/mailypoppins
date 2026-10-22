//! `mp signature` over the daemon's `signature.*` family (the daemon
//! leftovers' TUI follow-up, 2026-10-22).
//!
//! The family is the TUI's `cs` overlay's and the desktop's Signatures
//! dialog's; this pins the CLI's surface over it: what each subcommand prints,
//! that the default is per account and follows `-A`, that a rename carries the
//! default along and a delete clears it, and that a refusal is the daemon's
//! sentence on stderr with exit 1. The files are checked on disk, under the
//! sandbox's configuration directory, because that is where every client's
//! edit lands.

mod support;

use std::path::Path;
use std::process::Output;

use tempfile::TempDir;

use support::parity::{mp_command, DaemonFixture};

const CONFIG: &str = r#"
[[accounts]]
name = "alpha"
default_from = "alpha@example.com"

[accounts.mailboxes.inbox]
server = "INBOX"

[[accounts]]
name = "beta"
default_from = "beta@example.com"

[accounts.mailboxes.inbox]
server = "INBOX"
"#;

/// A sandbox with two accounts and a daemon serving it. The field order is
/// the drop order: the daemon goes before its root.
struct Sandbox {
    _daemon: DaemonFixture,
    tmp: TempDir,
}

impl Sandbox {
    fn start() -> Sandbox {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("config.toml"), CONFIG).unwrap();
        let daemon = DaemonFixture::start(tmp.path());
        Sandbox {
            _daemon: daemon,
            tmp,
        }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    fn run(&self, args: &[&str]) -> Output {
        mp_command(self.root())
            .env("EDITOR", "/usr/bin/true")
            .args(args)
            .output()
            .unwrap()
    }

    /// Run `args`, insist it succeeded, and answer its stdout.
    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "mp {args:?} failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

#[test]
fn mp_signature_manages_the_files_and_each_accounts_default_through_the_daemon() {
    let sandbox = Sandbox::start();

    assert_eq!(
        sandbox.ok(&["signature", "list"]),
        "No signatures (create one with `mp signature new <name>`)\n"
    );

    // `new` creates an empty file and prints where it is.
    let created = sandbox.ok(&["signature", "new", "work"]);
    let path = Path::new(created.trim());
    assert!(path.is_file(), "the daemon wrote {}", path.display());
    assert!(
        path.starts_with(sandbox.root()),
        "the file is under the sandbox's configuration directory: {}",
        path.display()
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), "");
    assert_eq!(
        sandbox.ok(&["signature", "show", "work", "--path"]).trim(),
        created.trim()
    );

    // `show` prints what is in the file, which is what `$EDITOR` writes.
    std::fs::write(path, "-- \nAlice").unwrap();
    assert_eq!(sandbox.ok(&["signature", "show", "work"]), "-- \nAlice\n");
    let edited = sandbox.ok(&["signature", "edit", "work"]);
    assert!(edited.contains("Signature 'work' edited"), "{edited}");

    // The default is per account: `-A` names it, the first one otherwise.
    assert_eq!(sandbox.ok(&["signature", "default"]), "(none)\n");
    let set = sandbox.ok(&["signature", "default", "work"]);
    assert!(set.contains("Default signature of alpha: 'work'"), "{set}");
    assert_eq!(sandbox.ok(&["signature", "list"]), "work  (default)\n");
    assert_eq!(
        sandbox.ok(&["-A", "beta", "signature", "default"]),
        "(none)\n"
    );
    assert_eq!(sandbox.ok(&["signature", "list", "-A", "beta"]), "work\n");
    let listed: serde_json::Value =
        serde_json::from_str(&sandbox.ok(&["signature", "list", "--json"])).unwrap();
    assert_eq!(
        listed,
        serde_json::json!({"account": "alpha", "names": ["work"], "default": "work"})
    );

    // A rename carries the default along; a delete clears it.
    sandbox.ok(&["signature", "rename", "work", "office"]);
    assert_eq!(sandbox.ok(&["signature", "list"]), "office  (default)\n");
    assert!(!path.exists(), "the old file is gone");
    sandbox.ok(&["signature", "new", "casual"]);
    sandbox.ok(&["signature", "delete", "office"]);
    assert_eq!(sandbox.ok(&["signature", "list"]), "casual\n");
    assert_eq!(sandbox.ok(&["signature", "default"]), "(none)\n");

    // `--none` clears a default that is set.
    sandbox.ok(&["signature", "default", "casual"]);
    let cleared = sandbox.ok(&["signature", "default", "--none"]);
    assert!(
        cleared.contains("alpha has no default signature"),
        "{cleared}"
    );
    assert_eq!(sandbox.ok(&["signature", "default"]), "(none)\n");
}

#[test]
fn a_refused_signature_call_is_the_daemons_sentence_and_exit_1() {
    let sandbox = Sandbox::start();
    let out = sandbox.run(&["signature", "show", "missing"]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no signature named 'missing'"), "{stderr}");

    sandbox.ok(&["signature", "new", "work"]);
    let taken = sandbox.run(&["signature", "new", "work"]);
    assert_eq!(taken.status.code(), Some(1));
    assert!(!String::from_utf8_lossy(&taken.stderr).trim().is_empty());

    let unknown = sandbox.run(&["-A", "ghost", "signature", "list"]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("no account named 'ghost'"),
        "{}",
        String::from_utf8_lossy(&unknown.stderr)
    );
}
