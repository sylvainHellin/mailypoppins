//! The seeded drafts directory the Phase 4 draft slice is measured against
//! (plan P4-U5).
//!
//! `mp new`, `mp list`, `mp validate`, `mp mark-approved`, `mp mark-draft`,
//! `mp path`, `mp edit`, `mp reply`, `mp forward` and the bare-selector dry run
//! all answer from one account directory, so they are all compared against one
//! fixture. It is [`read_fixture`] plus a drafts directory: the messages a
//! reply or a forward is built from are store rows, and seeding them twice
//! would give the two slices two different definitions of "the message with
//! attachments".
//!
//! # What the fixture has, and why
//!
//! Five drafts for `alpha`, one for `beta`, each state the slice has to tell
//! apart present once:
//!
//! | axis | where |
//! |---|---|
//! | a valid, sendable draft | [`VALID`] (`angebot.md`) |
//! | an approved draft | [`APPROVED`] (`freigabe.md`) |
//! | a sent draft | [`SENT`] (`verschickt.md`) |
//! | a draft that parses and does not validate | [`NO_SUBJECT`] (`ohne-betreff.md`) |
//! | a file whose frontmatter will not parse | `kaputt.md`, which has no id at all |
//! | a draft only the second account holds | [`BETA_DRAFT`] (`notiz.md`) |
//!
//! Every seeded id is written into the file, so nothing here depends on a
//! minted one: `store::drafts::refresh` mints an id for a file that has none
//! *and writes it back*, which would make two runs of the same command over
//! the same fixture disagree. `kaputt.md` is the deliberate exception, and it
//! is exempt because a file that will not parse is never assigned an id -- it
//! is reported as skipped instead (#0080), which is the output under test.
//!
//! The ids are hand-written in the shape [`mailypoppins::store::drafts::new_id`]
//! mints: sixteen hex characters, the first one a letter, which is the one
//! shape that cannot be read back out of YAML as a number (#0077).
//!
//! # Pristine runs
//!
//! Half the slice writes: `mp new`, `mp reply` and `mp forward` create files,
//! `mp mark-approved` and `mp mark-draft` rewrite one line. A parity
//! comparison of a writing command therefore cannot simply run the two
//! binaries one after the other over one root -- the second would see the
//! first one's work. [`Stash`] takes a copy of every drafts directory before a
//! run and puts it back afterwards, so each binary sees the same fixture and
//! the comparison stays literal, over one root, with the paths in the output
//! identical on both sides.
//!
//! Restoring puts the *modification times* back too, and touches only the
//! files that actually changed. `store::drafts::list` orders by
//! `mtime DESC, id ASC`, so a restore that rewrote all five files would hand
//! the second binary a different listing order than the first one saw, and the
//! parity failure would be an artefact of the harness.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::read_fixture;

/// The account every draft command answers about without `-A`.
pub const ACCOUNT: &str = read_fixture::ACCOUNT;

/// The second configured account, reachable only by naming it.
pub const OTHER_ACCOUNT: &str = read_fixture::OTHER_ACCOUNT;

/// The configured account with no store on disk.
pub const STORELESS_ACCOUNT: &str = read_fixture::STORELESS_ACCOUNT;

/// An account name no configuration carries.
pub const UNKNOWN_ACCOUNT: &str = read_fixture::UNKNOWN_ACCOUNT;

/// The accounts whose drafts directory the fixture seeds, in config order.
pub const SEEDED_ACCOUNTS: [&str; 2] = [ACCOUNT, OTHER_ACCOUNT];

/// A valid, sendable draft: recipient, subject and body all present.
pub const VALID: &str = "a0000000000000d1";

/// A draft already in `approved` status.
pub const APPROVED: &str = "b0000000000000a1";

/// A draft already `sent`, which may be neither approved nor demoted.
pub const SENT: &str = "c0000000000000e1";

/// A draft whose frontmatter parses and whose content does not validate: no
/// `subject:`, which `draft::validate_draft` refuses.
pub const NO_SUBJECT: &str = "d000000000000051";

/// The draft only [`OTHER_ACCOUNT`] holds.
pub const BETA_DRAFT: &str = "e0000000000000b1";

/// An id in the minted shape that nothing in the fixture resolves to.
pub const UNKNOWN_ID: &str = "f0000000000000ff";

/// The file that will not parse, which therefore has no id and no row.
pub const UNPARSEABLE_FILE: &str = "kaputt.md";

/// `<root>/accounts/<account>/drafts`, the directory
/// [`mailypoppins::config::drafts_dir`] resolves to under a sandbox root.
pub fn drafts_dir(root: &Path, account: &str) -> PathBuf {
    read_fixture::account_dir(root, account).join("drafts")
}

/// The path of one seeded draft.
pub fn draft_path(root: &Path, account: &str, file: &str) -> PathBuf {
    drafts_dir(root, account).join(file)
}

/// Write the read fixture's configuration and stores, then the drafts.
///
/// Call it before starting a daemon against the same root: the daemon loads
/// `config.toml` once, at startup.
pub fn seed(root: &Path) {
    read_fixture::seed(root);
    seed_drafts(root);
}

fn seed_drafts(root: &Path) {
    let alpha = drafts_dir(root, ACCOUNT);
    fs::create_dir_all(&alpha).unwrap_or_else(|e| panic!("create {}: {e}", alpha.display()));

    write(
        &alpha.join("angebot.md"),
        &document(VALID, "robin@example.com", "Angebot", "draft", "Body.\n"),
    );
    write(
        &alpha.join("freigabe.md"),
        &document(
            APPROVED,
            "ivana@example.com",
            "Freigabe",
            "approved",
            "Bereit.\n",
        ),
    );
    write(
        &alpha.join("verschickt.md"),
        &document(
            SENT,
            "petzold@example.com",
            "Verschickt",
            "sent",
            "Schon raus.\n",
        ),
    );
    // Parses, does not validate: `validate_draft` refuses an empty subject
    // before it looks at anything else.
    write(
        &alpha.join("ohne-betreff.md"),
        &document(
            NO_SUBJECT,
            "robin@example.com",
            "",
            "draft",
            "Kein Betreff.\n",
        ),
    );
    // The quote on the `subject:` line is never closed, so the YAML does not
    // scan and the file is skipped with a diagnostic rather than indexed.
    write(
        &alpha.join(UNPARSEABLE_FILE),
        "---\nid: e0000000000000c1\nto: robin@example.com\nsubject: \"Ohne Ende\nstatus: draft\n---\n\nBody.\n",
    );

    let beta = drafts_dir(root, OTHER_ACCOUNT);
    fs::create_dir_all(&beta).unwrap_or_else(|e| panic!("create {}: {e}", beta.display()));
    write(
        &beta.join("notiz.md"),
        &document(BETA_DRAFT, "beta@example.com", "Notiz", "draft", "Beta.\n"),
    );
}

/// One draft file, in the frontmatter shape `draft::parse_email_draft` reads.
pub fn document(id: &str, to: &str, subject: &str, status: &str, body: &str) -> String {
    format!(
        "---\n\
         id: {id}\n\
         to: {to}\n\
         cc:\n\
         bcc:\n\
         subject: \"{subject}\"\n\
         status: {status}\n\
         from: alpha@example.com\n\
         date: 2026-07-01 09:00\n\
         ---\n\
         \n\
         {body}"
    )
}

/// The canonical selector of a draft, which is what every draft command
/// prints.
pub fn selector(account: &str, id: &str) -> String {
    format!("mp://{account}/drafts/{id}")
}

fn write(path: &Path, content: &str) {
    fs::write(path, content).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

// ---------------------------------------------------------------------------
// Pristine runs
// ---------------------------------------------------------------------------

/// A copy of every seeded drafts directory, restorable in place.
///
/// Restoring puts back exactly the files that were there: a file a command
/// created is removed, a file a command rewrote gets its bytes back. Nothing
/// touches the store, because every draft command rebuilds the drafts index
/// from the directory before it reads it.
pub struct Stash {
    dirs: Vec<PathBuf>,
    files: BTreeMap<PathBuf, Saved>,
}

/// One stashed file: its bytes and the modification time the index sorts by.
struct Saved {
    bytes: Vec<u8>,
    modified: std::time::SystemTime,
}

impl Stash {
    /// Take a copy of `root`'s drafts directories.
    pub fn take(root: &Path) -> Stash {
        let dirs: Vec<PathBuf> = SEEDED_ACCOUNTS
            .iter()
            .map(|account| drafts_dir(root, account))
            .collect();
        let mut files = BTreeMap::new();
        for dir in &dirs {
            for path in markdown_files(dir) {
                let bytes =
                    fs::read(&path).unwrap_or_else(|e| panic!("stash {}: {e}", path.display()));
                let modified = fs::metadata(&path)
                    .and_then(|meta| meta.modified())
                    .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()));
                files.insert(path, Saved { bytes, modified });
            }
        }
        Stash { dirs, files }
    }

    /// Put the drafts directories back the way [`Stash::take`] found them.
    pub fn restore(&self) {
        for dir in &self.dirs {
            for path in markdown_files(dir) {
                if !self.files.contains_key(&path) {
                    fs::remove_file(&path)
                        .unwrap_or_else(|e| panic!("remove {}: {e}", path.display()));
                }
            }
        }
        for (path, saved) in &self.files {
            if fs::read(path).is_ok_and(|current| current == saved.bytes) {
                continue;
            }
            let parent = path.parent().expect("a draft path has a parent");
            fs::create_dir_all(parent)
                .unwrap_or_else(|e| panic!("create {}: {e}", parent.display()));
            fs::write(path, &saved.bytes)
                .unwrap_or_else(|e| panic!("restore {}: {e}", path.display()));
            let file = fs::OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap_or_else(|e| panic!("reopen {}: {e}", path.display()));
            let times = fs::FileTimes::new().set_modified(saved.modified);
            file.set_times(times)
                .unwrap_or_else(|e| panic!("restore the mtime of {}: {e}", path.display()));
        }
    }

    /// The files that are there now and were not when the stash was taken, in
    /// path order: what the command under test created.
    pub fn created(&self, root: &Path) -> Vec<PathBuf> {
        SEEDED_ACCOUNTS
            .iter()
            .flat_map(|account| markdown_files(&drafts_dir(root, account)))
            .filter(|path| !self.files.contains_key(path))
            .collect()
    }
}

/// Every depth-1 `*.md` file of `dir`, in name order, or nothing when the
/// directory is not there.
fn markdown_files(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "md"))
            .collect(),
        Err(_) => Vec::new(),
    };
    paths.sort();
    paths
}
