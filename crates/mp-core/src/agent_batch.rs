//! The batch files a hand-off writes for the coding agent (PERSO-315): the
//! TUI and the desktop app each write the batch prompt to a file named
//! `batch-<stamp>.md` under `<data_dir>/notes/` and start the agent on it,
//! then prune the ones an agent has long read, with this one rule.

use std::path::Path;
use std::time::{Duration, SystemTime};

/// How long a batch file is kept: an agent has long read it by then.
pub const BATCH_KEPT_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Whether `name` is a batch file's, `batch-<stamp>.md` with a stamp of
/// digits and dashes, as both clients name them.
pub fn is_batch_name(name: &str) -> bool {
    name.strip_prefix("batch-")
        .and_then(|rest| rest.strip_suffix(".md"))
        .is_some_and(|stamp| {
            !stamp.is_empty() && stamp.chars().all(|c| c.is_ascii_digit() || c == '-')
        })
}

/// Delete the batch files in `dir` last written more than
/// [`BATCH_KEPT_FOR`] before `now`. Only regular files go: any other name, a
/// symlink and a directory stay, and a file that cannot be read or removed is
/// left alone.
pub fn prune_batches(dir: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_str().is_some_and(is_batch_name) {
            continue;
        }
        // `DirEntry::metadata` does not follow a symlink, so one is no file.
        let old = entry
            .metadata()
            .ok()
            .filter(|meta| meta.is_file())
            .and_then(|meta| meta.modified().ok())
            .and_then(|written| now.duration_since(written).ok())
            .is_some_and(|age| age > BATCH_KEPT_FOR);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The batch files older than a week go; a recent one, another name, a
    /// look-alike and a symlink to an old batch stay.
    #[test]
    fn pruning_deletes_only_the_batch_files_older_than_a_week() {
        let dir = tempfile::tempdir().expect("a scratch dir");
        let elsewhere = tempfile::tempdir().expect("a scratch dir");
        let day = Duration::from_secs(24 * 60 * 60);
        let aged = |at: &Path, name: &str, days: u32| {
            let path = at.join(name);
            std::fs::write(&path, "x").unwrap();
            let file = std::fs::File::options().write(true).open(&path).unwrap();
            file.set_modified(SystemTime::now() - day * days).unwrap();
            path
        };
        let old = aged(dir.path(), "batch-20261101-090000.md", 8);
        let old_desktop = aged(dir.path(), "batch-2026-11-01-1761987600000.md", 8);
        let recent = aged(dir.path(), "batch-20261112-090000.md", 6);
        let other = aged(dir.path(), "notes-20261101.md", 30);
        let lookalike = aged(dir.path(), "batch-mine.md", 30);
        let linked = dir.path().join("batch-20261020-090000.md");
        #[cfg(unix)]
        std::os::unix::fs::symlink(aged(elsewhere.path(), "kept.md", 30), &linked).unwrap();
        prune_batches(dir.path(), SystemTime::now());
        assert!(!old.exists() && !old_desktop.exists(), "the old batches go");
        assert!(recent.exists());
        assert!(other.exists() && lookalike.exists(), "only batch files go");
        #[cfg(unix)]
        assert!(linked.symlink_metadata().is_ok(), "a symlink stays");
        assert!(is_batch_name("batch-20261116-101500.md"));
        assert!(is_batch_name("batch-2026-11-16-1763287200000.md"));
        assert!(!is_batch_name("batch-.md"));
        assert!(!is_batch_name("batch-20261116.md.bak"));
    }
}
