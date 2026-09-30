//! Where the daemon lives on disk, as the GUI finds it.
//!
//! The data and config directories come from `mp_core::config`, the same
//! functions the `mp` binary resolves them with, so `$MAILYPOPPINS_DATA_DIR`,
//! `$MAILYPOPPINS_CONFIG_DIR` and the platform defaults (`~/Library/Application
//! Support/mailypoppins` on macOS, `$XDG_DATA_HOME/mailypoppins` or
//! `~/.local/share/mailypoppins` on Linux) cannot drift between the two.
//!
//! The runtime directory, the socket and the daemon log are derived here
//! because their owners (`src/daemon/runtime/mod.rs` and
//! `src/daemon/lifecycle.rs`) live in the root crate, which a client may not
//! link. The `*_in` functions take the data directory so tests exercise the
//! derivation without touching the process environment.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// `$MAILYPOPPINS_DATA_DIR`, or the platform default.
pub fn data_dir() -> PathBuf {
    mp_core::config::mailypoppins_data_dir()
}

/// `$MAILYPOPPINS_CONFIG_DIR`, or `~/.config/mailypoppins`.
pub fn config_dir() -> PathBuf {
    mp_core::config::config_dir()
}

/// `<data>/runtime`, mirroring `daemon::runtime::runtime_dir`.
pub fn runtime_dir_in(data: &Path) -> PathBuf {
    data.join("runtime")
}

/// `<data>/runtime/daemon.sock`, mirroring `daemon::runtime::socket_path`.
pub fn socket_path_in(data: &Path) -> PathBuf {
    runtime_dir_in(data).join("daemon.sock")
}

/// `<data>/logs`, mirroring `mp_core::config::logs_dir`.
pub fn logs_dir_in(data: &Path) -> PathBuf {
    data.join("logs")
}

/// `<data>/logs/daemon.log`, mirroring `daemon::lifecycle::daemon_log_path`:
/// where `mp daemon start` points a detached daemon's stdio.
pub fn daemon_log_path_in(data: &Path) -> PathBuf {
    logs_dir_in(data).join("daemon.log")
}

/// `<data>/logs/mp-desktop.log`, the GUI's own trace log beside the daemon's.
pub fn desktop_log_path_in(data: &Path) -> PathBuf {
    logs_dir_in(data).join("mp-desktop.log")
}

/// Every path the GUI needs, resolved once from the environment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Paths {
    pub data_dir: PathBuf,
    pub config_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub socket: PathBuf,
    pub logs_dir: PathBuf,
    pub daemon_log: PathBuf,
    pub desktop_log: PathBuf,
}

impl Paths {
    /// Resolve from the process environment.
    pub fn resolve() -> Paths {
        Paths::from_dirs(data_dir(), config_dir())
    }

    /// Derive everything from a data and a config directory.
    pub fn from_dirs(data_dir: PathBuf, config_dir: PathBuf) -> Paths {
        Paths {
            runtime_dir: runtime_dir_in(&data_dir),
            socket: socket_path_in(&data_dir),
            logs_dir: logs_dir_in(&data_dir),
            daemon_log: daemon_log_path_in(&data_dir),
            desktop_log: desktop_log_path_in(&data_dir),
            data_dir,
            config_dir,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::env_lock;

    /// Set or clear one variable for the life of the guard.
    struct EnvVar {
        name: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVar {
        fn set(name: &'static str, value: Option<&str>) -> EnvVar {
            let previous = std::env::var_os(name);
            match value {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
            EnvVar { name, previous }
        }
    }

    impl Drop for EnvVar {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(v) => std::env::set_var(self.name, v),
                None => std::env::remove_var(self.name),
            }
        }
    }

    #[test]
    fn the_data_dir_override_moves_every_derived_path() {
        let _lock = env_lock();
        let _data = EnvVar::set("MAILYPOPPINS_DATA_DIR", Some("/tmp/mp-gui-paths"));
        let paths = Paths::resolve();
        assert_eq!(paths.data_dir, PathBuf::from("/tmp/mp-gui-paths"));
        assert_eq!(
            paths.runtime_dir,
            PathBuf::from("/tmp/mp-gui-paths/runtime")
        );
        assert_eq!(
            paths.socket,
            PathBuf::from("/tmp/mp-gui-paths/runtime/daemon.sock")
        );
        assert_eq!(
            paths.daemon_log,
            PathBuf::from("/tmp/mp-gui-paths/logs/daemon.log")
        );
        assert_eq!(
            paths.desktop_log,
            PathBuf::from("/tmp/mp-gui-paths/logs/mp-desktop.log")
        );
    }

    #[test]
    fn a_tilde_in_the_override_is_expanded_like_the_binary_does() {
        let _lock = env_lock();
        let _data = EnvVar::set("MAILYPOPPINS_DATA_DIR", Some("~/mp-gui-data"));
        let home = std::env::var("HOME").unwrap_or_default();
        assert_eq!(data_dir(), PathBuf::from(home).join("mp-gui-data"));
    }

    #[test]
    fn an_empty_override_falls_back_to_the_platform_default() {
        let _lock = env_lock();
        let _data = EnvVar::set("MAILYPOPPINS_DATA_DIR", Some(""));
        let _xdg = EnvVar::set("XDG_DATA_HOME", None);
        let dir = data_dir();
        #[cfg(target_os = "macos")]
        assert!(
            dir.ends_with("Library/Application Support/mailypoppins"),
            "{}",
            dir.display()
        );
        #[cfg(target_os = "linux")]
        assert!(
            dir.ends_with(".local/share/mailypoppins"),
            "{}",
            dir.display()
        );
        assert!(dir.ends_with("mailypoppins"));
    }

    #[test]
    fn the_config_dir_override_is_independent_of_the_data_dir() {
        let _lock = env_lock();
        let _data = EnvVar::set("MAILYPOPPINS_DATA_DIR", Some("/tmp/mp-gui-data"));
        let _config = EnvVar::set("MAILYPOPPINS_CONFIG_DIR", Some("/tmp/mp-gui-config"));
        let paths = Paths::resolve();
        assert_eq!(paths.config_dir, PathBuf::from("/tmp/mp-gui-config"));
        assert_eq!(paths.data_dir, PathBuf::from("/tmp/mp-gui-data"));
    }

    #[test]
    fn the_derivation_matches_the_daemon_layout() {
        let data = Path::new("/d");
        assert_eq!(
            socket_path_in(data),
            PathBuf::from("/d/runtime/daemon.sock")
        );
        assert_eq!(
            daemon_log_path_in(data),
            PathBuf::from("/d/logs/daemon.log")
        );
    }
}
