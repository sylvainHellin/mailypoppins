//! Where a routed send anchors a draft's relative `attachments:` entries
//! (#0123, the P4-U15 review).
//!
//! Every other user-supplied path is absolutised by the *client* before it
//! crosses the socket (`daemon::client::absolutise`), because the client is
//! the process with a meaningful working directory. An attachment entry cannot
//! follow that rule: no attachment path crosses the wire at all - `send.draft`
//! carries a selector, and the daemon reads the draft file itself - so there
//! is nothing for the client to rewrite short of editing the user's draft.
//!
//! And the daemon's own cwd is nobody's choice: `daemon::lifecycle`'s
//! `spawn_detached` sets no `current_dir`, so an auto-started daemon inherits
//! whichever directory the first client happened to stand in and keeps it for
//! its whole life. A `current_dir()`-anchored entry would therefore resolve
//! somewhere the user never named.
//!
//! So a relative entry is anchored to **the draft file's own directory**,
//! which is the one location both processes agree on
//! (`send::resolve_attachment_paths` takes the anchor explicitly). That is an
//! accepted divergence from the pre-daemon binary, which anchored to the
//! sender's cwd; it is recorded in `docs/tickets/0123-cli-cutover.md`,
//! `docs/parity-matrix.md` and `docs/daemon-protocol.md`.
//!
//! Both rows below run the client from a third directory - neither the draft's
//! nor the daemon's - and the daemon is started from `/`. So a cwd-anchored
//! resolution, on either side, resolves to a file that is not there:
//!
//! - the positive row would fail to read the attachment and exit nonzero;
//! - the negative row would name a path under `/` or under the client's cwd
//!   rather than under the drafts directory.
//!
//! Routed side only: both use `MAILYPOPPINS_DAEMON_FAKE_TRANSPORT`, which the
//! pre-daemon oracle knows nothing about, so neither is a parity row.

mod support;

use std::fs;
use std::path::Path;
use std::process::Output;

use tempfile::TempDir;

use support::parity::{mp_command, DaemonFixture, REQUIRE_ENV};
use support::send_fixture as fixture;

/// The id of the draft these rows send, in the minted shape, and distinct from
/// every id the fixture seeds.
const ATTACHING: &str = "b1000000000000e1";

/// The file it lives in, in [`fixture::ACCOUNT`]'s drafts directory.
const ATTACHING_FILE: &str = "anhang.md";

/// The attachment entry the draft names: relative, one path component, no
/// `~`, which is the entry the anchor decides the meaning of.
const ENTRY: &str = "report.pdf";

/// A relative entry no file anywhere satisfies, so the failure names the
/// anchor.
const MISSING_ENTRY: &str = "nicht-da.pdf";

/// A seeded root, a daemon started from `/` over it, and a client directory
/// that is neither.
struct Slice {
    daemon: Option<DaemonFixture>,
    elsewhere: TempDir,
    tmp: TempDir,
}

impl Slice {
    /// Seed the root, add the attaching draft (and its attachment, when
    /// `attachment` is `Some`), then start a daemon from `/` over it.
    ///
    /// In that order: the daemon loads `config.toml` once, at startup.
    fn start(entry: &str, attachment: Option<&str>) -> Slice {
        let tmp = TempDir::new().expect("a temporary attachment-slice root");
        fixture::seed(tmp.path());

        let drafts = fixture::drafts_dir(tmp.path(), fixture::ACCOUNT);
        let draft = drafts.join(ATTACHING_FILE);
        fs::write(&draft, attaching_document(entry))
            .unwrap_or_else(|e| panic!("write {}: {e}", draft.display()));
        if let Some(name) = attachment {
            let file = drafts.join(name);
            fs::write(&file, b"%PDF-1.4 the one the draft names")
                .unwrap_or_else(|e| panic!("write {}: {e}", file.display()));
        }

        let hook = fixture::fake_transport(&fixture::transport_log(tmp.path()));
        let daemon = DaemonFixture::start_with(
            tmp.path(),
            Some(Path::new("/")),
            &[(fixture::FAKE_TRANSPORT_ENV, &hook)],
        );
        Slice {
            daemon: Some(daemon),
            elsewhere: TempDir::new().expect("a client directory of its own"),
            tmp,
        }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    /// The routed client, run from a directory that is neither the draft's nor
    /// the daemon's.
    fn routed_elsewhere(&self, args: &[&str]) -> Output {
        mp_command(self.root())
            .env(REQUIRE_ENV, "1")
            .current_dir(self.elsewhere.path())
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("run routed `mp {}`: {e}", args.join(" ")))
    }
}

impl Drop for Slice {
    fn drop(&mut self) {
        // Best-effort and without asserting: a panic while another panic
        // unwinds aborts the process and hides the assertion the row was
        // about.
        if let Some(daemon) = self.daemon.take() {
            daemon.stop();
        }
    }
}

/// The attaching draft: approved, one recipient the fake transport accepts,
/// and one `attachments:` entry.
fn attaching_document(entry: &str) -> String {
    format!(
        "---\n\
         id: {ATTACHING}\n\
         to: ivana@example.com\n\
         subject: \"Mit Anhang\"\n\
         status: approved\n\
         from: alpha@example.com\n\
         attachments:\n  - \"{entry}\"\n\
         date: 2026-07-01 09:00\n\
         ---\n\
         \n\
         Anbei.\n"
    )
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// A draft whose relative attachment sits next to it sends, with the client
/// standing somewhere else entirely and the daemon started from `/`.
#[test]
fn a_relative_attachment_beside_the_draft_sends_from_any_working_directory() {
    let slice = Slice::start(ENTRY, Some(ENTRY));
    let selector = fixture::selector(fixture::ACCOUNT, ATTACHING);

    let out = slice.routed_elsewhere(&["send", &selector, "-y"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "the attachment resolved next to the draft rather than against a cwd:\n\
         stdout: {}\nstderr: {}",
        stdout(&out),
        stderr(&out)
    );
    assert!(
        stdout(&out).contains("✓ Email sent successfully to all"),
        "the success line:\n{}",
        stdout(&out)
    );

    // Non-vacuous: the fake transport really served this message, so the send
    // got past building the MIME body, which is where the attachment is read.
    let events = fixture::transport_events(&fixture::transport_log(slice.root()));
    assert!(
        !events.is_empty(),
        "the fake transport served the send rather than the send being skipped"
    );
}

/// And when no such file is beside the draft, the failure names the draft's
/// own directory: the anchor, said out loud.
#[test]
fn a_relative_attachment_that_is_nowhere_fails_naming_the_drafts_directory() {
    let slice = Slice::start(MISSING_ENTRY, None);
    let selector = fixture::selector(fixture::ACCOUNT, ATTACHING);

    let out = slice.routed_elsewhere(&["send", &selector, "-y"]);
    assert_ne!(
        out.status.code(),
        Some(0),
        "a missing attachment is a failed send:\nstdout: {}\nstderr: {}",
        stdout(&out),
        stderr(&out)
    );

    let expected = fixture::drafts_dir(slice.root(), fixture::ACCOUNT).join(MISSING_ENTRY);
    let text = format!("{}{}", stdout(&out), stderr(&out));
    assert!(
        text.contains(&expected.display().to_string()),
        "the failure names the path the entry resolved to, under the draft's own directory \
         ({}), and not one under the client's cwd or the daemon's:\n{text}",
        expected.display()
    );
}
