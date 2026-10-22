//! `mp -A <account>` with no subcommand opens the TUI on that account (the
//! daemon leftovers' TUI follow-up, 2026-10-22).
//!
//! The TUI's half, opening on the named account, is `App::open_on_account`'s
//! unit test; what is pinned here is the binary's half, which needs no
//! terminal: a name no account carries is refused before the daemon is asked
//! for a session and before the terminal is touched, with the sentence every
//! other `-A` refusal prints.

mod support;

use tempfile::TempDir;

use support::parity::mp_command;

const CONFIG: &str = r#"
[[accounts]]
name = "alpha"
default_from = "alpha@example.com"

[accounts.mailboxes.inbox]
server = "INBOX"
"#;

#[test]
fn the_tui_refuses_an_account_dash_a_does_not_name_before_it_starts() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("config.toml"), CONFIG).unwrap();
    let out = mp_command(tmp.path())
        // No daemon is started for this run, and none may be: the refusal
        // comes before the session.
        .env("MAILYPOPPINS_DAEMON_AUTOSTART", "0")
        .args(["-A", "ghost"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("no account named 'ghost'"), "{stderr}");
    assert!(
        !tmp.path().join("runtime").join("daemon.sock").exists(),
        "no daemon was started for a launch that was refused"
    );
}
