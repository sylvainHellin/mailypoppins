//! Runtime paths, the startup lock, and stale-socket handling for the daemon
//! (#0120, unit P2-U4).
//!
//! This file is a **contract test**: it is written before `src/daemon/` has any
//! contents, against the API fixed in
//! `.agents/workflow/native-gui-daemon/plan.md` section 3.3 (unit P2-U4), and
//! it compiles only under `--features daemon` so the pre-daemon tree keeps
//! building. Until P2-U5 lands, `cargo test --workspace --features daemon` must
//! fail with unresolved-import errors naming exactly `mailypoppins::daemon`
//! items and nothing else. An implementer does not edit this file; they make it
//! pass.
//!
//! The file layout under test is pinned in plan section 3.0:
//!
//! ```text
//! <data_dir>/runtime/                    dir mode 0700
//! <data_dir>/runtime/daemon.sock         socket mode 0600
//! <data_dir>/runtime/daemon.start.lock   flock target, never unlinked
//! <data_dir>/runtime/daemon.pid          diagnostic only, never the lock
//! <data_dir>/runtime/daemon.json         InstanceMeta
//! ```
//!
//! # Why a mutex
//!
//! Every contract function resolves its path through
//! `config::mailypoppins_data_dir()`, which reads `$MAILYPOPPINS_DATA_DIR`, and
//! none of them takes a path argument. An integration test therefore has to
//! move the process environment, which is global to the binary and to every
//! thread in it. [`ENV_LOCK`] serialises the whole body of every test in this
//! file so that no other test thread is reading the environment while one of
//! them writes it; [`DataDirGuard`] restores the previous values before it
//! releases the lock. Poisoning is ignored on purpose: one failing assertion
//! must not cascade into a dozen misleading `PoisonError` failures.
//!
//! # Two contract points this file could not exercise
//!
//! - A socket owned by **another uid** needs a second uid, which a test cannot
//!   create. The group/other-bits half of the same `Unsafe` rule is asserted
//!   for real; the foreign-owner half is `#[ignore]`d with the reason on the
//!   attribute and needs a root runner.
//! - Peer-credential checks on a *live* socket belong to the server unit, not
//!   here. This file only classifies the path.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier, Mutex, MutexGuard, OnceLock};
use std::thread;

use serde_json::Value;
use tempfile::TempDir;

use mailypoppins::daemon::runtime::{
    acquire_start_lock, ensure_runtime_dir, instance_path, pid_path, probe_socket,
    remove_stale_socket, runtime_dir, socket_path, start_lock_path, InstanceMeta, SocketProbe,
    StartLock,
};

// ---------------------------------------------------------------------------
// Environment harness
// ---------------------------------------------------------------------------

/// Serialises the tests in this binary, because they all move
/// `$MAILYPOPPINS_DATA_DIR`.
fn env_lock() -> MutexGuard<'static, ()> {
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Points `$MAILYPOPPINS_DATA_DIR` (and `$MAILYPOPPINS_CONFIG_DIR`, so a stray
/// read of the real one cannot leak into `InstanceMeta`) at a fresh `TempDir`
/// for the duration of one test, holding [`env_lock`] the whole time.
struct DataDirGuard {
    tmp: TempDir,
    prev_data: Option<OsString>,
    prev_config: Option<OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl DataDirGuard {
    fn new() -> Self {
        let lock = env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev_data = std::env::var_os("MAILYPOPPINS_DATA_DIR");
        let prev_config = std::env::var_os("MAILYPOPPINS_CONFIG_DIR");
        // The data root itself exists; only `runtime/` is the daemon's to make.
        fs::create_dir_all(tmp.path().join("data")).expect("create data root");
        fs::create_dir_all(tmp.path().join("config")).expect("create config root");
        std::env::set_var("MAILYPOPPINS_DATA_DIR", tmp.path().join("data"));
        std::env::set_var("MAILYPOPPINS_CONFIG_DIR", tmp.path().join("config"));
        Self {
            tmp,
            prev_data,
            prev_config,
            _lock: lock,
        }
    }

    /// `$MAILYPOPPINS_DATA_DIR` for this test.
    fn data_dir(&self) -> PathBuf {
        self.tmp.path().join("data")
    }

    /// `$MAILYPOPPINS_CONFIG_DIR` for this test.
    fn config_dir(&self) -> PathBuf {
        self.tmp.path().join("config")
    }
}

impl Drop for DataDirGuard {
    fn drop(&mut self) {
        match self.prev_data.take() {
            Some(v) => std::env::set_var("MAILYPOPPINS_DATA_DIR", v),
            None => std::env::remove_var("MAILYPOPPINS_DATA_DIR"),
        }
        match self.prev_config.take() {
            Some(v) => std::env::set_var("MAILYPOPPINS_CONFIG_DIR", v),
            None => std::env::remove_var("MAILYPOPPINS_CONFIG_DIR"),
        }
    }
}

/// The permission bits of `path`, masked to the usual twelve.
fn mode_of(path: &Path) -> u32 {
    fs::metadata(path)
        .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()))
        .permissions()
        .mode()
        & 0o7777
}

/// A rendering of a [`SocketProbe`] for assertion messages.
///
/// Written by hand rather than through `{:?}` so this file does not silently
/// require `SocketProbe: Debug`; the contract in the plan derives nothing.
fn describe(probe: &SocketProbe) -> String {
    match probe {
        SocketProbe::Live => "Live".to_string(),
        SocketProbe::Stale => "Stale".to_string(),
        SocketProbe::Absent => "Absent".to_string(),
        SocketProbe::Unsafe { reason } => format!("Unsafe {{ reason: {reason:?} }}"),
    }
}

/// Binds a listener at `path` and tightens the socket to 0600, which is the
/// mode the plan pins and the only mode `probe_socket` may call safe.
fn bind_socket(path: &Path) -> UnixListener {
    let listener =
        UnixListener::bind(path).unwrap_or_else(|e| panic!("bind {}: {e}", path.display()));
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("chmod socket 0600");
    listener
}

/// A socket file whose listener is gone: bound, tightened, then the listener
/// dropped. `std`'s `UnixListener` does not unlink on drop, so the file stays
/// behind with nobody accepting on it, which is exactly what a crashed daemon
/// leaves.
fn leave_stale_socket(path: &Path) {
    let listener = bind_socket(path);
    drop(listener);
    assert!(
        path.exists(),
        "test precondition: dropping a UnixListener must leave {} behind",
        path.display()
    );
}

// ---------------------------------------------------------------------------
// Path resolution
// ---------------------------------------------------------------------------

/// `runtime_dir()` is `<data_dir>/runtime`, and it follows
/// `$MAILYPOPPINS_DATA_DIR` rather than the real user's data root.
#[test]
fn runtime_dir_is_the_runtime_subdirectory_of_the_data_dir() {
    let env = DataDirGuard::new();
    assert_eq!(
        runtime_dir(),
        env.data_dir().join("runtime"),
        "runtime_dir() must be <data_dir>/runtime and must honour $MAILYPOPPINS_DATA_DIR"
    );
}

/// The four file names inside the runtime directory are pinned by plan section
/// 3.0 and tests may hard-code them.
#[test]
fn the_runtime_file_names_are_fixed() {
    let env = DataDirGuard::new();
    let runtime = env.data_dir().join("runtime");

    assert_eq!(socket_path(), runtime.join("daemon.sock"));
    assert_eq!(start_lock_path(), runtime.join("daemon.start.lock"));
    assert_eq!(pid_path(), runtime.join("daemon.pid"));
    assert_eq!(instance_path(), runtime.join("daemon.json"));

    let distinct: BTreeSet<PathBuf> = [
        socket_path(),
        start_lock_path(),
        pid_path(),
        instance_path(),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        distinct.len(),
        4,
        "the socket, the start lock, the pid file and the instance file are four distinct paths"
    );
}

// ---------------------------------------------------------------------------
// ensure_runtime_dir
// ---------------------------------------------------------------------------

/// A missing runtime directory is created user-only.
#[test]
fn ensure_runtime_dir_creates_the_directory_with_mode_0700() {
    let env = DataDirGuard::new();
    assert!(
        !env.data_dir().join("runtime").exists(),
        "test precondition: the runtime directory must not exist yet"
    );

    let created = ensure_runtime_dir().expect("ensure_runtime_dir on a missing directory");

    assert_eq!(created, runtime_dir(), "the returned path is runtime_dir()");
    assert!(
        created.is_dir(),
        "{} must be a directory",
        created.display()
    );
    assert_eq!(
        mode_of(&created),
        0o700,
        "the runtime directory is user-only: the socket and the instance metadata live in it"
    );
}

/// A directory left at 0755 by an older build, a lax umask, or a user's `mkdir`
/// is tightened rather than accepted.
#[test]
fn ensure_runtime_dir_tightens_a_loose_directory_to_0700() {
    let env = DataDirGuard::new();
    let runtime = env.data_dir().join("runtime");
    fs::create_dir_all(&runtime).expect("pre-create runtime");
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).expect("chmod 0755");
    assert_eq!(mode_of(&runtime), 0o755, "test precondition");

    let returned = ensure_runtime_dir().expect("ensure_runtime_dir on a 0755 directory");

    assert_eq!(returned, runtime);
    assert_eq!(
        mode_of(&runtime),
        0o700,
        "a loose runtime directory must be tightened, not tolerated"
    );
}

/// Group-writable is the worst case and must also come back tightened, and a
/// second call on an already-correct directory is a no-op rather than an error.
#[test]
fn ensure_runtime_dir_is_idempotent_and_tightens_group_and_other_bits() {
    let env = DataDirGuard::new();
    let runtime = env.data_dir().join("runtime");
    fs::create_dir_all(&runtime).expect("pre-create runtime");
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o777)).expect("chmod 0777");

    ensure_runtime_dir().expect("first call");
    assert_eq!(mode_of(&runtime), 0o700, "0777 must be tightened to 0700");

    ensure_runtime_dir().expect("second call must not fail on an existing 0700 directory");
    assert_eq!(mode_of(&runtime), 0o700, "the second call changes nothing");
}

// ---------------------------------------------------------------------------
// probe_socket
// ---------------------------------------------------------------------------

/// Nothing at the path is `Absent`, which is the "start a daemon" signal and
/// must not be confused with `Stale`.
#[test]
fn a_missing_path_probes_absent() {
    let _env = DataDirGuard::new();
    ensure_runtime_dir().expect("ensure_runtime_dir");
    let path = socket_path();

    let probe = probe_socket(&path);
    assert!(
        matches!(probe, SocketProbe::Absent),
        "a missing socket must probe Absent, got {}",
        describe(&probe)
    );
}

/// A socket file with no listener is `Stale`: the connect attempt proves no
/// daemon is there, and the mode and owner prove it is ours.
#[test]
fn a_socket_with_no_listener_probes_stale() {
    let _env = DataDirGuard::new();
    ensure_runtime_dir().expect("ensure_runtime_dir");
    let path = socket_path();
    leave_stale_socket(&path);

    let probe = probe_socket(&path);
    assert!(
        matches!(probe, SocketProbe::Stale),
        "an unlistened, user-owned 0600 socket must probe Stale, got {}",
        describe(&probe)
    );
    assert!(
        path.exists(),
        "probe_socket never unlinks: it classifies only"
    );
}

/// The only path on which `remove_stale_socket` succeeds.
#[test]
fn a_stale_socket_is_removed() {
    let _env = DataDirGuard::new();
    ensure_runtime_dir().expect("ensure_runtime_dir");
    let path = socket_path();
    leave_stale_socket(&path);

    remove_stale_socket(&path).expect("a Stale, user-owned socket must be removable");

    assert!(
        !path.exists(),
        "remove_stale_socket must unlink the socket it accepted"
    );
    let probe = probe_socket(&path);
    assert!(
        matches!(probe, SocketProbe::Absent),
        "after removal the path probes Absent, got {}",
        describe(&probe)
    );
}

/// A daemon is listening: the path is `Live` and unlinking it would cut every
/// future client off from a healthy daemon.
#[test]
fn a_live_socket_probes_live_and_is_refused_by_remove_stale_socket() {
    let _env = DataDirGuard::new();
    ensure_runtime_dir().expect("ensure_runtime_dir");
    let path = socket_path();
    let _listener = bind_socket(&path);

    let probe = probe_socket(&path);
    assert!(
        matches!(probe, SocketProbe::Live),
        "a socket with a listener must probe Live, got {}",
        describe(&probe)
    );

    assert!(
        remove_stale_socket(&path).is_err(),
        "remove_stale_socket must refuse a Live socket"
    );
    assert!(
        path.exists(),
        "a refused remove_stale_socket must leave the socket in place"
    );
}

/// Group or other bits on the socket mean something else can reach the daemon,
/// so the path is `Unsafe` even when no listener answers: `Unsafe` outranks
/// `Stale`, and an unsafe path is never unlinked because we cannot prove it is
/// the file we think it is.
#[test]
fn a_socket_with_group_or_other_bits_probes_unsafe_and_is_never_removed() {
    for loose in [0o660, 0o606, 0o666, 0o700 | 0o007] {
        let _env = DataDirGuard::new();
        ensure_runtime_dir().expect("ensure_runtime_dir");
        let path = socket_path();
        leave_stale_socket(&path);
        fs::set_permissions(&path, fs::Permissions::from_mode(loose)).expect("chmod loose");

        let probe = probe_socket(&path);
        match &probe {
            SocketProbe::Unsafe { reason } => assert!(
                !reason.trim().is_empty(),
                "mode {loose:o}: Unsafe must carry a reason a user can act on"
            ),
            other => panic!(
                "mode {loose:o}: a socket reachable by group or other must probe Unsafe, got {}",
                describe(other)
            ),
        }

        assert!(
            remove_stale_socket(&path).is_err(),
            "mode {loose:o}: remove_stale_socket must refuse an Unsafe socket"
        );
        assert!(
            path.exists(),
            "mode {loose:o}: a refused remove_stale_socket must leave the file in place"
        );
    }
}

/// The other-uid half of the same rule. A test process cannot create a file
/// owned by a uid it does not have, so this one is ignored by default and needs
/// a root runner: `sudo -E cargo test --features daemon
/// --test daemon_runtime_paths -- --ignored`. The group/other-bits variant
/// above carries the same contract for a non-root run.
#[test]
#[ignore = "needs root: a test process cannot create a file owned by another uid"]
fn a_socket_owned_by_another_uid_probes_unsafe_and_is_never_removed() {
    let _env = DataDirGuard::new();
    ensure_runtime_dir().expect("ensure_runtime_dir");
    let path = socket_path();
    leave_stale_socket(&path);

    let chowned = std::process::Command::new("chown")
        .arg("1:1")
        .arg(&path)
        .status()
        .expect("spawn chown");
    assert!(
        chowned.success(),
        "this test must run as root; chown 1:1 {} failed",
        path.display()
    );

    let probe = probe_socket(&path);
    match &probe {
        SocketProbe::Unsafe { reason } => assert!(
            !reason.trim().is_empty(),
            "Unsafe must carry a reason a user can act on"
        ),
        other => panic!(
            "a socket owned by another uid must probe Unsafe, got {}",
            describe(other)
        ),
    }

    assert!(
        remove_stale_socket(&path).is_err(),
        "remove_stale_socket must refuse a socket owned by another uid"
    );
    assert!(
        path.exists(),
        "a refused remove_stale_socket must leave the file in place"
    );
}

/// A regular file where the socket belongs is not a stale socket: connecting to
/// it fails, but unlinking an arbitrary file the user put there is not ours to
/// do.
#[test]
fn a_regular_file_at_the_socket_path_probes_unsafe() {
    let _env = DataDirGuard::new();
    ensure_runtime_dir().expect("ensure_runtime_dir");
    let path = socket_path();
    fs::write(&path, b"not a socket").expect("write regular file");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod 0600");

    let probe = probe_socket(&path);
    match &probe {
        SocketProbe::Unsafe { reason } => assert!(
            !reason.trim().is_empty(),
            "Unsafe must carry a reason a user can act on"
        ),
        other => panic!(
            "a regular file at the socket path must probe Unsafe, got {}",
            describe(other)
        ),
    }

    assert!(
        remove_stale_socket(&path).is_err(),
        "remove_stale_socket must refuse a path that is not a stale socket"
    );
    assert!(path.exists(), "the user's file must survive");
}

/// `remove_stale_socket` refuses unless the probe says `Stale`, and `Absent` is
/// not `Stale`. Deleting nothing is cheap; a caller that reaches here has lost
/// track of its own state and must hear about it.
#[test]
fn remove_stale_socket_refuses_a_path_that_does_not_exist() {
    let _env = DataDirGuard::new();
    ensure_runtime_dir().expect("ensure_runtime_dir");

    assert!(
        remove_stale_socket(&socket_path()).is_err(),
        "remove_stale_socket must refuse an Absent path: only Stale is removable"
    );
}

// ---------------------------------------------------------------------------
// acquire_start_lock
// ---------------------------------------------------------------------------

/// The whole point of the startup lock: two starters race, one wins, the other
/// is told to wait for the winner rather than spawning a second daemon. Both
/// threads hold their result across a barrier, so the loser genuinely contends
/// with a held lock instead of sneaking in after the winner released it.
#[test]
fn two_threads_racing_for_the_start_lock_yield_exactly_one_some() {
    let _env = DataDirGuard::new();
    ensure_runtime_dir().expect("ensure_runtime_dir");

    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let guard: Option<StartLock> =
                    acquire_start_lock().expect("acquire_start_lock must not error on contention");
                let won = guard.is_some();
                // Hold the lock until both threads have tried.
                barrier.wait();
                drop(guard);
                won
            })
        })
        .collect();

    let winners = handles
        .into_iter()
        .map(|h| h.join().expect("start-lock thread panicked"))
        .filter(|won| *won)
        .count();

    assert_eq!(
        winners, 1,
        "exactly one of two concurrent acquire_start_lock() calls may return Some; \
         Ok(None) is how the loser learns another starter holds it"
    );
    assert!(
        start_lock_path().exists(),
        "the start lock file is created by the first acquisition"
    );
}

/// The lock's lifetime is the guard's: once it drops, the next starter wins.
#[test]
fn the_start_lock_is_released_when_the_guard_drops() {
    let _env = DataDirGuard::new();
    ensure_runtime_dir().expect("ensure_runtime_dir");

    let first: Option<StartLock> = acquire_start_lock().expect("first acquisition");
    assert!(
        first.is_some(),
        "an uncontended acquire_start_lock() must return Some"
    );
    drop(first);

    let second: Option<StartLock> = acquire_start_lock().expect("second acquisition");
    assert!(
        second.is_some(),
        "dropping the guard must release the lock for the next starter"
    );
}

/// Plan section 3.0: the flock target is never unlinked. Unlinking it would let
/// two starters lock two different inodes under the same name and both proceed.
#[test]
fn the_start_lock_file_outlives_its_guard() {
    let _env = DataDirGuard::new();
    ensure_runtime_dir().expect("ensure_runtime_dir");

    let guard: Option<StartLock> = acquire_start_lock().expect("acquire");
    assert!(guard.is_some());
    assert!(
        start_lock_path().exists(),
        "the lock file exists while held"
    );
    drop(guard);

    assert!(
        start_lock_path().exists(),
        "daemon.start.lock is never unlinked: releasing the lock must not remove the file"
    );
}

// ---------------------------------------------------------------------------
// InstanceMeta
// ---------------------------------------------------------------------------

fn sample_meta(data_dir: PathBuf, config_dir: PathBuf) -> InstanceMeta {
    InstanceMeta {
        app_version: "0.9.0".to_string(),
        protocol_min: 1,
        protocol_max: 1,
        instance_id: "0d2f7f7a-4c1e-4a5e-9b6e-0f3a1c2d4e5f".to_string(),
        pid: 4242,
        started_at: "2025-01-31T09:15:00Z".to_string(),
        data_dir,
        config_dir,
    }
}

/// `daemon.json` is what `mp daemon status` reads out of a running daemon, so
/// every field has to survive the file.
#[test]
fn instance_meta_round_trips_through_daemon_json() {
    let env = DataDirGuard::new();
    ensure_runtime_dir().expect("ensure_runtime_dir");
    let meta = sample_meta(env.data_dir(), env.config_dir());

    let json = serde_json::to_string_pretty(&meta).expect("serialise InstanceMeta");
    fs::write(instance_path(), &json).expect("write daemon.json");

    let raw = fs::read_to_string(instance_path()).expect("read daemon.json");
    let back: InstanceMeta = serde_json::from_str(&raw).expect("deserialise InstanceMeta");

    assert_eq!(back.app_version, meta.app_version);
    assert_eq!(back.protocol_min, meta.protocol_min);
    assert_eq!(back.protocol_max, meta.protocol_max);
    assert_eq!(back.instance_id, meta.instance_id);
    assert_eq!(back.pid, meta.pid);
    assert_eq!(back.started_at, meta.started_at);
    assert_eq!(back.data_dir, meta.data_dir);
    assert_eq!(back.config_dir, meta.config_dir);
}

/// The on-disk key names are the struct's field names, flat, with no rename.
/// A client reads this file directly when no daemon answers, so the names are
/// wire surface and moving one is a protocol-changelog change.
#[test]
fn instance_meta_is_a_flat_json_object_with_the_contract_field_names() {
    let env = DataDirGuard::new();
    let meta = sample_meta(env.data_dir(), env.config_dir());

    let value: Value = serde_json::to_value(&meta).expect("serialise InstanceMeta");
    let object = value
        .as_object()
        .expect("InstanceMeta serialises to a JSON object");

    let keys: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    let expected: BTreeSet<&str> = [
        "app_version",
        "protocol_min",
        "protocol_max",
        "instance_id",
        "pid",
        "started_at",
        "data_dir",
        "config_dir",
    ]
    .into_iter()
    .collect();
    assert_eq!(
        keys, expected,
        "daemon.json carries exactly the eight contract fields, unrenamed"
    );

    assert!(value["protocol_min"].is_u64(), "protocol_min is a number");
    assert!(value["protocol_max"].is_u64(), "protocol_max is a number");
    assert!(value["pid"].is_u64(), "pid is a number");
    assert_eq!(
        value["data_dir"],
        Value::String(env.data_dir().to_string_lossy().into_owned()),
        "data_dir serialises as the path string, so a client can compare it with its own"
    );
    assert_eq!(
        value["config_dir"],
        Value::String(env.config_dir().to_string_lossy().into_owned()),
        "config_dir serialises as the path string"
    );
}
