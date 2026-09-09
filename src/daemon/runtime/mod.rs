//! Runtime paths, the startup lock, and stale-socket handling (P2-U4/P2-U5).
//!
//! Everything the daemon needs on disk lives in one user-only directory below
//! the data root, so that pointing `$MAILYPOPPINS_DATA_DIR` at a tempdir moves
//! a whole daemon instance with it:
//!
//! ```text
//! <data_dir>/runtime/                    dir mode 0700
//! <data_dir>/runtime/daemon.sock         socket mode 0600
//! <data_dir>/runtime/daemon.start.lock   flock target, never unlinked
//! <data_dir>/runtime/daemon.pid          diagnostic only, never the lock
//! <data_dir>/runtime/daemon.json         InstanceMeta
//! ```
//!
//! ## Why the start lock is a separate file from the pid file
//!
//! The lock is an advisory `flock` whose lifetime is the file descriptor's, so
//! the kernel releases it on exit however the process died. A pid file cannot
//! do that: a reader finds a number that may name a dead process or, worse, a
//! recycled one. `daemon.pid` is therefore diagnostic output and
//! `daemon.start.lock` is the primitive. The lock file is **never unlinked**:
//! two starters that each locked a different inode under the same name would
//! both win.
//!
//! ## What else lives here
//!
//! [`account`] is one account's runtime - the engine lock it holds for its
//! lifetime, the tick it runs - and [`pool`] is the read pool that runtime
//! serves from. Both arrived in P3b-U4; this module was a single file until
//! then, and the paths and the start lock below are unchanged by the move.
//!
//! ## Why a probe classifies instead of deciding
//!
//! [`probe_socket`] never touches the filesystem beyond reading metadata and
//! attempting one connection. Only [`remove_stale_socket`] unlinks, and only
//! for the single classification that proves the file is ours and dead. A path
//! we cannot vouch for is [`SocketProbe::Unsafe`] and stays where it is: a
//! daemon that deletes a file it does not understand is a daemon that deletes
//! user data.

use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use log::debug;
use serde::{Deserialize, Serialize};

pub mod account;
pub mod pool;

/// Mode of the runtime directory: nobody but the owner may even list it.
const RUNTIME_DIR_MODE: u32 = 0o700;

/// The only mode a socket may carry and still be considered ours. Anything
/// else, including a stray execute bit, is [`SocketProbe::Unsafe`].
const SOCKET_MODE: u32 = 0o600;

/// `<data_dir>/runtime`, following `$MAILYPOPPINS_DATA_DIR`.
pub fn runtime_dir() -> PathBuf {
    crate::config::mailypoppins_data_dir().join("runtime")
}

/// `<runtime>/daemon.sock`, the local socket clients connect to.
pub fn socket_path() -> PathBuf {
    runtime_dir().join("daemon.sock")
}

/// `<runtime>/daemon.start.lock`, the `flock` target that serialises starts.
pub fn start_lock_path() -> PathBuf {
    runtime_dir().join("daemon.start.lock")
}

/// `<runtime>/daemon.pid`, diagnostic metadata and never a locking primitive.
pub fn pid_path() -> PathBuf {
    runtime_dir().join("daemon.pid")
}

/// `<runtime>/daemon.json`, the [`InstanceMeta`] a client reads when no daemon
/// answers the socket.
pub fn instance_path() -> PathBuf {
    runtime_dir().join("daemon.json")
}

/// Create [`runtime_dir`] if it is missing and make sure it is mode 0700,
/// returning the directory.
///
/// Idempotent, and it *tightens*: a directory left group-readable by an older
/// build, a lax umask or a hand-rolled `mkdir` is fixed rather than tolerated,
/// because the socket and the instance metadata inside it are user-only.
pub fn ensure_runtime_dir() -> Result<PathBuf> {
    let dir = runtime_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating the daemon runtime directory {}", dir.display()))?;

    let mode = fs::metadata(&dir)
        .with_context(|| format!("reading the mode of {}", dir.display()))?
        .permissions()
        .mode()
        & 0o7777;
    if mode != RUNTIME_DIR_MODE {
        fs::set_permissions(&dir, fs::Permissions::from_mode(RUNTIME_DIR_MODE)).with_context(
            || {
                format!(
                    "tightening {} from {mode:04o} to {RUNTIME_DIR_MODE:04o}",
                    dir.display()
                )
            },
        )?;
        debug!(
            "[daemon] tightened {} from {mode:04o} to {RUNTIME_DIR_MODE:04o}",
            dir.display()
        );
    }
    Ok(dir)
}

/// A held advisory lock on [`start_lock_path`].
///
/// The `File` is kept alive purely to keep the fd open: `flock` is released by
/// `close(2)`, so the lock lives exactly as long as this value and not one
/// instruction longer, whether the starter returns, panics, or is killed.
#[derive(Debug)]
pub struct StartLock {
    _file: fs::File,
}

/// Try to take the startup lock, non-blocking.
///
/// - `Ok(Some(guard))`: this process may go on to bind the socket.
/// - `Ok(None)`: another starter holds the lock; wait for its daemon to become
///   ready instead of spawning a second one.
/// - `Err`: the lock file could not be opened, or `flock` failed for a reason
///   other than contention.
pub fn acquire_start_lock() -> Result<Option<StartLock>> {
    ensure_runtime_dir()?;
    let path = start_lock_path();
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(SOCKET_MODE)
        .open(&path)
        .with_context(|| format!("opening the daemon start lock {}", path.display()))?;

    // SAFETY: `flock` takes a valid open file descriptor and a flag set; the fd
    // is owned by `file` and outlives the call. `LOCK_NB` makes it return
    // immediately rather than block behind the winning starter.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        debug!("[daemon] took the start lock {}", path.display());
        return Ok(Some(StartLock { _file: file }));
    }

    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        // Both spellings a kernel may use for "already locked". Contention is
        // the expected outcome of a race, not a failure.
        Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => {
            debug!("[daemon] another starter holds {}", path.display());
            Ok(None)
        }
        _ => Err(anyhow::Error::new(err).context(format!("locking {}", path.display()))),
    }
}

/// What is at a socket path, as far as we can prove.
pub enum SocketProbe {
    /// A daemon accepted a connection. Never unlink this.
    Live,
    /// Our own 0600 socket with nobody listening: a crashed daemon's leftover,
    /// and the only thing [`remove_stale_socket`] will delete.
    Stale,
    /// Something we cannot vouch for: not a socket, not ours, or reachable by
    /// group or other. Outranks [`SocketProbe::Stale`], and is never unlinked.
    Unsafe { reason: String },
    /// Nothing is there. Bind and serve.
    Absent,
}

/// Classify `path` by inspecting its metadata and then attempting one
/// connection. Never creates, changes or removes anything.
///
/// The metadata checks run first, so an unsafe path is reported as unsafe
/// whether or not something answers on it.
pub fn probe_socket(path: &Path) -> SocketProbe {
    // `symlink_metadata`, not `metadata`: a symlink at the socket path points
    // the unlink and the connect at two different questions, and we answer
    // neither by following it.
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return SocketProbe::Absent,
        Err(e) => {
            return SocketProbe::Unsafe {
                reason: format!("cannot stat {}: {e}", path.display()),
            }
        }
    };

    let file_type = meta.file_type();
    if !file_type.is_socket() {
        let kind = if file_type.is_symlink() {
            "a symlink"
        } else if file_type.is_dir() {
            "a directory"
        } else {
            "a regular file"
        };
        return SocketProbe::Unsafe {
            reason: format!(
                "{} is {kind}, not a socket; move it aside and retry",
                path.display()
            ),
        };
    }

    // SAFETY: `geteuid` takes no argument and cannot fail.
    let uid = unsafe { libc::geteuid() };
    if meta.uid() != uid {
        return SocketProbe::Unsafe {
            reason: format!(
                "{} is owned by uid {}, not by uid {uid}; refusing to use or remove it",
                path.display(),
                meta.uid()
            ),
        };
    }

    let mode = meta.permissions().mode() & 0o7777;
    if mode != SOCKET_MODE {
        return SocketProbe::Unsafe {
            reason: format!(
                "{} has mode {mode:04o}, not {SOCKET_MODE:04o}; another user may be able to \
                 reach the daemon, so it is neither used nor removed",
                path.display()
            ),
        };
    }

    match UnixStream::connect(path) {
        Ok(_) => SocketProbe::Live,
        Err(e) => match e.kind() {
            // Nobody is accepting: the file outlived its daemon.
            io::ErrorKind::ConnectionRefused => SocketProbe::Stale,
            // Somebody removed it between the stat and the connect.
            io::ErrorKind::NotFound => SocketProbe::Absent,
            _ => SocketProbe::Unsafe {
                reason: format!("connecting to {} failed: {e}", path.display()),
            },
        },
    }
}

/// Unlink `path`, but only when [`probe_socket`] says [`SocketProbe::Stale`].
///
/// Every other classification is an error, including [`SocketProbe::Absent`]:
/// deleting nothing is cheap, but a caller that got here has lost track of its
/// own state and must hear about it rather than proceed on a wrong belief.
pub fn remove_stale_socket(path: &Path) -> Result<()> {
    match probe_socket(path) {
        SocketProbe::Stale => {
            fs::remove_file(path)
                .with_context(|| format!("removing the stale socket {}", path.display()))?;
            debug!("[daemon] removed the stale socket {}", path.display());
            Ok(())
        }
        SocketProbe::Live => bail!(
            "{} has a daemon listening on it; refusing to remove a live socket",
            path.display()
        ),
        SocketProbe::Absent => bail!(
            "{} does not exist; only a stale socket can be removed",
            path.display()
        ),
        SocketProbe::Unsafe { reason } => bail!("refusing to remove {}: {reason}", path.display()),
    }
}

/// What a running daemon publishes in `daemon.json`.
///
/// A client that gets no answer on the socket reads this file directly, so the
/// key names are wire surface: they are the field names below, flat and
/// unrenamed, and moving one is a protocol-changelog change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceMeta {
    /// The daemon binary's crate version.
    pub app_version: String,
    /// Lowest protocol version this daemon speaks.
    pub protocol_min: u32,
    /// Highest protocol version this daemon speaks.
    pub protocol_max: u32,
    /// Fresh per daemon process, so a client can tell a restart from a reconnect.
    pub instance_id: String,
    /// Diagnostic; the start lock, not this, is what excludes a second daemon.
    pub pid: u32,
    /// RFC 3339, UTC.
    pub started_at: String,
    /// The data root this daemon was started against; a client compares it with
    /// its own and refuses to talk to a daemon holding a different one.
    pub data_dir: PathBuf,
    /// The config root, compared the same way.
    pub config_dir: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// The four runtime files are four distinct names below one directory, so a
    /// caller can never lock the file it is about to unlink.
    #[test]
    fn the_runtime_paths_are_distinct_children_of_the_runtime_directory() {
        let dir = runtime_dir();
        for path in [socket_path(), start_lock_path(), pid_path(), instance_path()] {
            assert_eq!(path.parent(), Some(dir.as_path()));
        }
        let names: std::collections::BTreeSet<_> =
            [socket_path(), start_lock_path(), pid_path(), instance_path()]
                .iter()
                .map(|p| p.file_name().unwrap().to_owned())
                .collect();
        assert_eq!(names.len(), 4);
    }

    /// `probe_socket` takes the path explicitly, so this needs no environment.
    #[test]
    fn probe_classifies_absent_stale_and_live_without_touching_the_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("daemon.sock");

        assert!(matches!(probe_socket(&path), SocketProbe::Absent));

        let listener = UnixListener::bind(&path).expect("bind");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
        assert!(matches!(probe_socket(&path), SocketProbe::Live));

        drop(listener);
        assert!(matches!(probe_socket(&path), SocketProbe::Stale));
        assert!(path.exists(), "probing never unlinks");
    }

    /// A directory where the socket belongs is unsafe, and the reason names the
    /// path so the user can act on it.
    #[test]
    fn a_directory_at_the_socket_path_is_unsafe() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("daemon.sock");
        fs::create_dir(&path).expect("mkdir");

        match probe_socket(&path) {
            SocketProbe::Unsafe { reason } => {
                assert!(reason.contains("daemon.sock"), "reason was {reason}");
            }
            _ => panic!("a directory at the socket path must probe Unsafe"),
        }
        assert!(remove_stale_socket(&path).is_err());
        assert!(path.is_dir());
    }
}
