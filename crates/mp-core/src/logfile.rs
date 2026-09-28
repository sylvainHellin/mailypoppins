//! The log files under `<data_dir>/logs/`, capped in size.
//!
//! Every `mp` process (a CLI call, the TUI, the daemon) appends to the same
//! dated file, `mailypoppins-YYYY-MM-DD.log`, through [`RotatingLog`]. Nothing
//! used to bound it: the daemon kept the file of the day it started open for
//! weeks, and a debug-level `html5ever` trace of every rendered message filled
//! gigabytes. Two things bound it now.
//!
//! - [`CrateLevels`] keeps `DEBUG` for this workspace's own crates and drops a
//!   dependency's records below `INFO`.
//! - [`RotatingLog`] moves to a fresh file when the UTC date changes or the
//!   current file reaches [`FILE_CAP_BYTES`], and every time it opens one it
//!   deletes the oldest log files until the directory is under
//!   [`TOTAL_CAP_BYTES`].
//!
//! A file rolled for size is `mailypoppins-YYYY-MM-DDTHHMMSSmmm.log`: `T` sorts
//! after `.`, so the lexicographic order [`crate::config::latest_log_file`]
//! relies on is still the chronological one.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};
use log::{LevelFilter, Log, Metadata, Record};

/// The size at which the current file is left for a fresh one.
pub const FILE_CAP_BYTES: u64 = 20 * 1024 * 1024;

/// What every `mailypoppins-*.log` file together may take on disk.
pub const TOTAL_CAP_BYTES: u64 = 200 * 1024 * 1024;

/// The size past which `daemon.log`, a detached daemon's stdio, is emptied.
///
/// Every writer opens it `O_APPEND`, so truncating it under a running daemon
/// is safe: the next write lands at offset 0.
pub const DAEMON_LOG_CAP_BYTES: u64 = 10 * 1024 * 1024;

const PREFIX: &str = "mailypoppins-";
const SUFFIX: &str = ".log";

/// The crates whose `DEBUG` records are worth keeping.
const OWN_CRATES: &[&str] = &["mailypoppins", "mp_core", "mp_tui", "mp_client", "mp_protocol"];

/// Whether `name` is one of the dated log files.
pub fn is_log_name(name: &str) -> bool {
    name.starts_with(PREFIX) && name.ends_with(SUFFIX)
}

/// Whether a record's target belongs to this workspace.
fn is_own_target(target: &str) -> bool {
    OWN_CRATES.iter().any(|krate| {
        target
            .strip_prefix(krate)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with("::"))
    })
}

/// The most verbose level kept for a record from `target`.
pub fn level_for(target: &str) -> LevelFilter {
    if is_own_target(target) {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    }
}

/// A logger that applies [`level_for`] before handing a record on.
pub struct CrateLevels<L> {
    inner: L,
}

impl<L: Log> CrateLevels<L> {
    pub fn new(inner: L) -> CrateLevels<L> {
        CrateLevels { inner }
    }
}

impl<L: Log> Log for CrateLevels<L> {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= level_for(metadata.target()) && self.inner.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            self.inner.log(record);
        }
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

/// A writer over the dated log files that rolls and prunes them.
pub struct RotatingLog {
    dir: PathBuf,
    file_cap: u64,
    total_cap: u64,
    file: File,
    path: PathBuf,
    day: NaiveDate,
    /// The file's size as last seen, plus what this process wrote since.
    size: u64,
    /// Records started since `size` was last read off the file, which other
    /// processes append to as well.
    records_since_stat: u32,
    /// Whether the last byte written ended a line. `simplelog` writes one
    /// record in several calls, and a roll between two of them would split
    /// the record across two files.
    at_line_start: bool,
}

/// How many records go by between two reads of the file's real size.
const RECORDS_PER_STAT: u32 = 256;

impl RotatingLog {
    /// Open the current file under `dir` with the default caps.
    pub fn open(dir: &Path) -> io::Result<RotatingLog> {
        RotatingLog::with_caps(dir, FILE_CAP_BYTES, TOTAL_CAP_BYTES, Utc::now())
    }

    /// Open the current file under `dir` as of `now`, then prune.
    pub fn with_caps(
        dir: &Path,
        file_cap: u64,
        total_cap: u64,
        now: DateTime<Utc>,
    ) -> io::Result<RotatingLog> {
        let (path, file, size) = open_current(dir, file_cap, now)?;
        let _ = prune(dir, total_cap, &path);
        Ok(RotatingLog {
            dir: dir.to_path_buf(),
            file_cap,
            total_cap,
            file,
            path,
            day: now.date_naive(),
            size,
            records_since_stat: 0,
            at_line_start: true,
        })
    }

    /// The file this writer appends to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Move to the file `now` calls for when the current one is done.
    ///
    /// A failure keeps the current file: a log that cannot roll still logs.
    fn roll_if_due(&mut self, now: DateTime<Utc>) {
        self.records_since_stat += 1;
        if self.records_since_stat >= RECORDS_PER_STAT {
            self.records_since_stat = 0;
            if let Ok(meta) = self.file.metadata() {
                self.size = meta.len();
            }
        }
        if self.size < self.file_cap && now.date_naive() == self.day {
            return;
        }
        if let Ok((path, file, size)) = open_current(&self.dir, self.file_cap, now) {
            let _ = prune(&self.dir, self.total_cap, &path);
            self.path = path;
            self.file = file;
            self.size = size;
            self.day = now.date_naive();
            self.records_since_stat = 0;
        }
    }
}

impl RotatingLog {
    fn write_at(&mut self, buf: &[u8], now: DateTime<Utc>) -> io::Result<usize> {
        if self.at_line_start && !buf.is_empty() {
            self.roll_if_due(now);
        }
        let n = self.file.write(buf)?;
        self.size += n as u64;
        if n > 0 {
            self.at_line_start = buf[n - 1] == b'\n';
        }
        Ok(n)
    }
}

impl Write for RotatingLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_at(buf, Utc::now())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// The file to append to as of `now`: the newest of the day while it is under
/// `file_cap`, a new one otherwise. Returns it with its current size.
fn open_current(dir: &Path, file_cap: u64, now: DateTime<Utc>) -> io::Result<(PathBuf, File, u64)> {
    let day = format!("{PREFIX}{}", now.format("%Y-%m-%d"));
    let newest_of_day = log_files(dir)?
        .into_iter()
        .filter(|(path, _)| file_name(path).is_some_and(|n| n.starts_with(&day)))
        .max_by(|a, b| a.0.cmp(&b.0));
    let path = match newest_of_day {
        Some((path, size)) if size < file_cap => path,
        Some(_) => dir.join(format!("{day}T{}{SUFFIX}", now.format("%H%M%S%3f"))),
        None => dir.join(format!("{day}{SUFFIX}")),
    };
    let file = OpenOptions::new().create(true).append(true).open(&path)?;
    let size = file.metadata()?.len();
    Ok((path, file, size))
}

/// Delete the oldest log files until the rest fit in `total_cap`, never
/// `keep`, and empty `daemon.log` once it is past [`DAEMON_LOG_CAP_BYTES`].
///
/// A file another process still writes survives as an unlinked inode until
/// that process rolls, which it does at the latest when the date changes.
pub fn prune(dir: &Path, total_cap: u64, keep: &Path) -> io::Result<()> {
    let mut files = log_files(dir)?;
    files.sort();
    let mut total: u64 = files.iter().map(|(_, size)| size).sum();
    for (path, size) in files {
        if total <= total_cap {
            break;
        }
        if path == keep {
            continue;
        }
        if fs::remove_file(&path).is_ok() {
            total -= size;
        }
    }
    let daemon_log = dir.join("daemon.log");
    if fs::metadata(&daemon_log).is_ok_and(|m| m.len() > DAEMON_LOG_CAP_BYTES) {
        OpenOptions::new().write(true).open(&daemon_log)?.set_len(0)?;
    }
    Ok(())
}

/// Every dated log file under `dir` with its size.
fn log_files(dir: &Path) -> io::Result<Vec<(PathBuf, u64)>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        if !file_name(&path).is_some_and(is_log_name) {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                files.push((path, meta.len()));
            }
        }
    }
    Ok(files)
}

fn file_name(path: &Path) -> Option<&str> {
    path.file_name().and_then(|n| n.to_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, m: u32, d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap()
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = log_files(dir)
            .unwrap()
            .into_iter()
            .map(|(p, _)| file_name(&p).unwrap().to_string())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn own_crates_keep_debug_and_dependencies_keep_info() {
        assert_eq!(level_for("mailypoppins"), LevelFilter::Debug);
        assert_eq!(level_for("mailypoppins::daemon::server"), LevelFilter::Debug);
        assert_eq!(level_for("mp_core::config"), LevelFilter::Debug);
        assert_eq!(level_for("mp_tui"), LevelFilter::Debug);
        assert_eq!(level_for("html5ever::tree_builder"), LevelFilter::Info);
        assert_eq!(level_for("mp_coreish"), LevelFilter::Info);
        assert_eq!(level_for("mailypoppins_extra"), LevelFilter::Info);
    }

    #[test]
    fn opens_the_dated_file_and_appends_to_it() {
        let tmp = tempfile::tempdir().unwrap();
        let mut log = RotatingLog::with_caps(tmp.path(), 1000, 10_000, at(2026, 9, 28, 10)).unwrap();
        writeln!(log, "one").unwrap();
        assert_eq!(names(tmp.path()), ["mailypoppins-2026-09-28.log"]);
        drop(log);
        let mut log = RotatingLog::with_caps(tmp.path(), 1000, 10_000, at(2026, 9, 28, 11)).unwrap();
        writeln!(log, "two").unwrap();
        let text = fs::read_to_string(tmp.path().join("mailypoppins-2026-09-28.log")).unwrap();
        assert_eq!(text, "one\ntwo\n");
    }

    #[test]
    fn a_full_file_rolls_to_a_later_sorting_name() {
        let tmp = tempfile::tempdir().unwrap();
        let mut log = RotatingLog::with_caps(tmp.path(), 10, 10_000, at(2026, 9, 28, 10)).unwrap();
        log.write_all(b"0123456789ab\n").unwrap();
        log.write_all(b"next\n").unwrap();
        let names = names(tmp.path());
        assert_eq!(names.len(), 2, "{names:?}");
        assert_eq!(names[0], "mailypoppins-2026-09-28.log");
        assert!(names[1].starts_with("mailypoppins-2026-09-28T"), "{names:?}");
        assert!(names[1].as_str() < "mailypoppins-2026-09-29.log");
        let rolled = fs::read_to_string(tmp.path().join(&names[1])).unwrap();
        assert_eq!(rolled, "next\n");
    }

    #[test]
    fn a_new_utc_day_rolls_to_the_new_dated_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut log = RotatingLog::with_caps(tmp.path(), 1000, 10_000, at(2026, 9, 28, 23)).unwrap();
        log.write_at(b"late\n", at(2026, 9, 28, 23)).unwrap();
        log.write_at(b"early\n", at(2026, 9, 29, 0)).unwrap();
        assert_eq!(
            names(tmp.path()),
            ["mailypoppins-2026-09-28.log", "mailypoppins-2026-09-29.log"]
        );
        assert_eq!(log.path(), tmp.path().join("mailypoppins-2026-09-29.log"));
    }

    #[test]
    fn a_record_split_across_writes_is_not_split_across_files() {
        let tmp = tempfile::tempdir().unwrap();
        let mut log = RotatingLog::with_caps(tmp.path(), 4, 10_000, at(2026, 9, 28, 10)).unwrap();
        log.write_all(b"2026 ").unwrap();
        log.write_all(b"[INFO] ").unwrap();
        log.write_all(b"message\n").unwrap();
        let text = fs::read_to_string(tmp.path().join("mailypoppins-2026-09-28.log")).unwrap();
        assert_eq!(text, "2026 [INFO] message\n");
    }

    #[test]
    fn opening_prunes_the_oldest_files_down_to_the_cap() {
        let tmp = tempfile::tempdir().unwrap();
        for day in ["01", "02", "03", "04"] {
            fs::write(tmp.path().join(format!("mailypoppins-2026-09-{day}.log")), [b'x'; 100]).unwrap();
        }
        fs::write(tmp.path().join("unrelated.txt"), [b'x'; 1000]).unwrap();
        let log = RotatingLog::with_caps(tmp.path(), 1000, 250, at(2026, 9, 28, 10)).unwrap();
        assert_eq!(
            names(tmp.path()),
            ["mailypoppins-2026-09-03.log", "mailypoppins-2026-09-04.log", "mailypoppins-2026-09-28.log"]
        );
        assert_eq!(log.path(), tmp.path().join("mailypoppins-2026-09-28.log"));
        assert!(tmp.path().join("unrelated.txt").exists());
    }

    #[test]
    fn prune_never_deletes_the_file_it_keeps() {
        let tmp = tempfile::tempdir().unwrap();
        let keep = tmp.path().join("mailypoppins-2026-09-01.log");
        fs::write(&keep, [b'x'; 500]).unwrap();
        prune(tmp.path(), 10, &keep).unwrap();
        assert!(keep.exists());
    }

    #[test]
    fn prune_empties_an_oversized_daemon_log() {
        let tmp = tempfile::tempdir().unwrap();
        let daemon_log = tmp.path().join("daemon.log");
        let file = File::create(&daemon_log).unwrap();
        file.set_len(DAEMON_LOG_CAP_BYTES + 1).unwrap();
        prune(tmp.path(), TOTAL_CAP_BYTES, &tmp.path().join("none.log")).unwrap();
        assert_eq!(fs::metadata(&daemon_log).unwrap().len(), 0);
    }
}
