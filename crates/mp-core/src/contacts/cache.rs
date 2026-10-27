//! JSON cache for the per-account contact index.
//!
//! Each account's index lives at `<account_dir>/contacts-cache.json` (where
//! `account_dir` resolves under `mailypoppins_data_dir()`).
//!
//! Every read-modify-write of the file runs under [`with_cache_lock`], and
//! every write replaces it atomically ([`save_cache`]), PERSO-234: a sync's
//! merge, a send's merge and a rebuild's commit once each loaded the file,
//! changed it and wrote it back with a plain write on a multi-threaded
//! runtime, so two of them overlapping lost one, and a reader could see half
//! a file.

use crate::contacts::types::ContactIndex;
use anyhow::{Context, Result};
use log::warn;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// What `save_rebuilt_cache` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheSave {
    /// The rebuilt index replaced whatever was on disk.
    Written,
    /// The rebuild produced no contacts and the cache on disk holds `kept` of
    /// them, so the write was refused.
    RefusedEmpty { kept: usize },
    /// The rebuild came back with `rebuilt` contacts against `kept` on disk,
    /// far enough below it to look like a partial read rather than a corpus
    /// that shrank, so the write was refused (#0067).
    RefusedShrunk { kept: usize, rebuilt: usize },
}

/// How much of the cached corpus a rebuild has to find to be believed.
///
/// A rebuild is a deletion (#0053), and the zero case is not the only way to
/// lose one: a store pruned mid-read, or one mailbox's rows missing, yields a
/// small-but-nonzero index that replaced a large one and reported success. A
/// real corpus does not lose four fifths of itself between two rebuilds, so
/// anything under this fraction is treated as a failed read.
const SHRINK_REFUSE_RATIO: f64 = 0.2;

/// Cache file for a given account lives inside that account's data directory.
pub fn cache_path(account_root: &Path) -> PathBuf {
    account_root.join("contacts-cache.json")
}

pub fn load_cache(account_root: &Path) -> Result<Option<ContactIndex>> {
    let path = cache_path(account_root);
    if !path.exists() {
        return Ok(None);
    }
    let data = fs::read_to_string(&path)
        .with_context(|| format!("reading contacts cache at {}", path.display()))?;
    let index: ContactIndex = serde_json::from_str(&data)
        .with_context(|| format!("parsing contacts cache at {}", path.display()))?;
    Ok(Some(index))
}

/// Persist a freshly rebuilt index, refusing to replace a populated cache with
/// an empty one.
///
/// The frecency corpus is accumulated by the send/sync hooks over months and a
/// rebuild is the only thing that can throw it away wholesale, so a rebuild
/// that comes back with nothing is treated as a failure to read rather than as
/// an account with no correspondents (#0053). Incremental writes go through
/// `save_cache` directly: they can only ever add.
pub fn save_rebuilt_cache(account_root: &Path, index: &ContactIndex) -> Result<CacheSave> {
    let rebuilt = index.contacts.len();
    let kept = cached_count(account_root);
    if kept > 0 {
        if rebuilt == 0 {
            warn!(
                "[contacts] refusing to overwrite {} cached contacts for '{}' with an empty rebuild ({})",
                kept,
                index.account,
                cache_path(account_root).display()
            );
            return Ok(CacheSave::RefusedEmpty { kept });
        }
        if (rebuilt as f64) < (kept as f64) * SHRINK_REFUSE_RATIO {
            warn!(
                "[contacts] refusing to overwrite {} cached contacts for '{}' with a rebuild that found only {} ({})",
                kept,
                index.account,
                rebuilt,
                cache_path(account_root).display()
            );
            return Ok(CacheSave::RefusedShrunk { kept, rebuilt });
        }
    }
    save_cache(account_root, index)?;
    Ok(CacheSave::Written)
}

/// How many contacts the cache on disk holds, for the guard's comparison.
///
/// An unreadable or corrupt cache counts as zero rather than as an error: the
/// guard exists to protect data, and making it fail the whole rebuild left the
/// user with a broken `mp contacts rebuild` and no way to repair it by
/// rebuilding (#0067). Zero kept means the guard cannot fire, so the fresh
/// index replaces the unparseable file.
fn cached_count(account_root: &Path) -> usize {
    match load_cache(account_root) {
        Ok(cached) => cached.map_or(0, |c| c.contacts.len()),
        Err(e) => {
            warn!(
                "[contacts] unreadable cache at {}, treating it as empty: {e:#}",
                cache_path(account_root).display()
            );
            0
        }
    }
}

/// Run `f` holding the lock of the cache under `account_root`, the one lock
/// every load-merge-save and every rebuild commit of that file takes.
///
/// Per process: every writer of the cache is the daemon (the CLI, the TUI and
/// the desktop app read and rebuild the index through it), and a lock file
/// would only add a second way to be wrong. Not reentrant, so `f` must not
/// take it again. A writer that panicked while holding it left a file that
/// was replaced atomically or not at all, so the lock is taken over rather
/// than poisoned.
pub fn with_cache_lock<T>(account_root: &Path, f: impl FnOnce() -> T) -> T {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let lock = {
        let mut locks = LOCKS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Arc::clone(locks.entry(cache_path(account_root)).or_default())
    };
    let _held = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    f()
}

/// Write `index` as the account's cache, replacing the file atomically: the
/// bytes go to a temporary file beside it, which is then renamed over it, so
/// a reader sees the old index or the new one and never a torn file.
///
/// A caller that read the cache and writes back what it changed holds
/// [`with_cache_lock`] across both.
pub fn save_cache(account_root: &Path, index: &ContactIndex) -> Result<()> {
    let path = cache_path(account_root);
    if let Some(parent) = path.parent() {
        crate::config::create_private_dir_all(parent)
            .with_context(|| format!("creating cache directory at {}", parent.display()))?;
    }
    let data = serde_json::to_string_pretty(index)?;
    let staged = path.with_extension(format!("json.tmp-{}", std::process::id()));
    fs::write(&staged, data)
        .with_context(|| format!("writing contacts cache at {}", staged.display()))?;
    fs::rename(&staged, &path).map_err(|e| {
        let _ = fs::remove_file(&staged);
        anyhow::Error::new(e).context(format!("replacing contacts cache at {}", path.display()))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contacts::types::{Contact, ContactSource};
    use std::collections::HashMap;

    fn index(addresses: &[&str]) -> ContactIndex {
        let mut contacts = HashMap::new();
        for addr in addresses {
            contacts.insert(
                (*addr).to_string(),
                Contact {
                    address: (*addr).to_string(),
                    display_name: String::new(),
                    sent_to: 1,
                    sent_cc: 0,
                    received: 0,
                    first_seen: "2026-01-01T00:00:00+00:00".into(),
                    last_seen: "2026-01-01T00:00:00+00:00".into(),
                    source: ContactSource::Local,
                },
            );
        }
        ContactIndex {
            account: "alice".into(),
            contacts,
            built_at: "2026-01-01T00:00:00+00:00".into(),
            extractor_version: crate::contacts::EXTRACTOR_VERSION,
            rows_through: 0,
        }
    }

    /// #0053: an empty rebuild never replaces a populated cache.
    #[test]
    fn an_empty_rebuild_does_not_overwrite_a_populated_cache() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(
            dir.path(),
            &index(&["alice@example.com", "bob@example.com"]),
        )
        .unwrap();

        let outcome = save_rebuilt_cache(dir.path(), &index(&[])).unwrap();

        assert_eq!(outcome, CacheSave::RefusedEmpty { kept: 2 });
        let kept = load_cache(dir.path()).unwrap().unwrap();
        assert_eq!(kept.contacts.len(), 2);
    }

    /// The guard only fires against a populated cache: a first build of an
    /// account with no correspondents still writes its empty index.
    #[test]
    fn an_empty_rebuild_writes_when_there_is_nothing_to_lose() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            save_rebuilt_cache(dir.path(), &index(&[])).unwrap(),
            CacheSave::Written
        );
        assert!(load_cache(dir.path()).unwrap().unwrap().contacts.is_empty());

        // And again over the empty cache it just wrote.
        assert_eq!(
            save_rebuilt_cache(dir.path(), &index(&[])).unwrap(),
            CacheSave::Written
        );
    }

    /// #0067: a rebuild that finds a fraction of the cached corpus looks like
    /// a partial read, and a partial read must not erode the corpus.
    #[test]
    fn a_partial_rebuild_does_not_erode_a_populated_cache() {
        let dir = tempfile::tempdir().unwrap();
        let many: Vec<String> = (0..100).map(|i| format!("c{i}@example.com")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        save_cache(dir.path(), &index(&many)).unwrap();

        let outcome = save_rebuilt_cache(dir.path(), &index(&many[..3])).unwrap();

        assert_eq!(
            outcome,
            CacheSave::RefusedShrunk {
                kept: 100,
                rebuilt: 3
            }
        );
        assert_eq!(load_cache(dir.path()).unwrap().unwrap().contacts.len(), 100);
    }

    /// The shrink guard is not a freeze: a corpus that lost a few contacts
    /// (unsubscribes, a pruned mailbox) still writes.
    #[test]
    fn a_modest_shrink_still_writes() {
        let dir = tempfile::tempdir().unwrap();
        let many: Vec<String> = (0..100).map(|i| format!("c{i}@example.com")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        save_cache(dir.path(), &index(&many)).unwrap();

        let outcome = save_rebuilt_cache(dir.path(), &index(&many[..80])).unwrap();

        assert_eq!(outcome, CacheSave::Written);
        assert_eq!(load_cache(dir.path()).unwrap().unwrap().contacts.len(), 80);
    }

    /// #0067: a corrupt cache counts as zero kept instead of failing the
    /// rebuild, so `mp contacts rebuild` can repair the file.
    #[test]
    fn a_corrupt_cache_does_not_fail_an_empty_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(cache_path(dir.path()), "{ not json").unwrap();

        assert_eq!(
            save_rebuilt_cache(dir.path(), &index(&[])).unwrap(),
            CacheSave::Written
        );
        assert!(load_cache(dir.path()).unwrap().unwrap().contacts.is_empty());
    }

    /// PERSO-234 review: a save replaces the file through a staged copy that
    /// is renamed over it, so a reader racing the write reads a whole index,
    /// and nothing staged is left behind.
    #[test]
    fn a_save_replaces_the_file_whole_and_leaves_nothing_staged() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(dir.path(), &index(&["alice@example.com"])).unwrap();
        let readers = std::thread::scope(|scope| {
            let reader = scope.spawn(|| {
                let mut reads = 0;
                for _ in 0..200 {
                    // A torn file would be a parse error here.
                    load_cache(dir.path())
                        .unwrap()
                        .expect("always a whole cache");
                    reads += 1;
                }
                reads
            });
            for n in 0..200 {
                let many: Vec<String> = (0..n % 50 + 1)
                    .map(|i| format!("c{i}@example.com"))
                    .collect();
                let many: Vec<&str> = many.iter().map(String::as_str).collect();
                save_cache(dir.path(), &index(&many)).unwrap();
            }
            reader.join().unwrap()
        });
        assert_eq!(readers, 200);
        let names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["contacts-cache.json"]);
    }

    /// A non-empty rebuild replaces whatever was there.
    #[test]
    fn a_populated_rebuild_replaces_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(dir.path(), &index(&["alice@example.com"])).unwrap();

        let outcome = save_rebuilt_cache(dir.path(), &index(&["bob@example.com"])).unwrap();

        assert_eq!(outcome, CacheSave::Written);
        let loaded = load_cache(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.contacts.len(), 1);
        assert!(loaded.contacts.contains_key("bob@example.com"));
    }
}
