//! Named profiles on the command line (PERSO-176): `mp --profile <name>` and
//! `MAILYPOPPINS_PROFILE` move the config and data roots under
//! `profiles/<name>`, the two together with an explicit directory variable are
//! refused, a bad name exits 2, and two profiles run two daemons side by side.
//!
//! Each run gets its own `HOME` and no directory variables, so the default
//! roots are `<home>/.config/mailypoppins` and
//! `<home>/.local/share/mailypoppins` and a profile is resolved from them.

mod support;

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

use mailypoppins::daemon::lifecycle::IDLE_EXIT_ENV;
use support::parity::{pgrep_line_for, AUTOSTART_ENV, AUTOSTART_TIMEOUT_ENV, DAEMON_ENV_HOOKS, MP};

/// A run of `mp` whose default roots live under `home`.
fn mp_in(home: &Path) -> Command {
    let mut cmd = Command::new(MP);
    cmd.env("HOME", home)
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("MAILYPOPPINS_DATA_DIR")
        .env_remove("MAILYPOPPINS_CONFIG_DIR");
    for hook in DAEMON_ENV_HOOKS {
        cmd.env_remove(hook);
    }
    cmd.stdin(Stdio::null());
    cmd
}

fn run(cmd: &mut Command) -> Output {
    cmd.output().expect("run mp")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn config_base(home: &Path) -> PathBuf {
    home.join(".config").join("mailypoppins")
}

fn data_base(home: &Path) -> PathBuf {
    home.join(".local").join("share").join("mailypoppins")
}

fn socket_under(data: &Path) -> PathBuf {
    data.join("runtime").join("daemon.sock")
}

#[test]
fn mp_profile_config_path_names_the_profile_dirs() {
    let home = TempDir::new().unwrap();
    let expected = config_base(home.path())
        .join("profiles")
        .join("demo")
        .join("config.toml");

    let out = run(mp_in(home.path()).args(["--profile", "demo", "config", "path"]));
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout).trim(), expected.display().to_string());

    // After the subcommand, as `-A` is written.
    let out = run(mp_in(home.path()).args(["config", "path", "--profile", "demo"]));
    assert_eq!(text(&out.stdout).trim(), expected.display().to_string());

    let out = run(mp_in(home.path())
        .env("MAILYPOPPINS_PROFILE", "demo")
        .args(["config", "path"]));
    assert_eq!(text(&out.stdout).trim(), expected.display().to_string());

    // The flag wins over the variable, and `default` is today's path.
    let out = run(mp_in(home.path())
        .env("MAILYPOPPINS_PROFILE", "demo")
        .args(["--profile", "default", "config", "path"]));
    assert_eq!(
        text(&out.stdout).trim(),
        config_base(home.path())
            .join("config.toml")
            .display()
            .to_string()
    );
}

#[test]
fn mp_profile_with_data_dir_is_refused_naming_both() {
    let home = TempDir::new().unwrap();
    let out = run(mp_in(home.path())
        .env("MAILYPOPPINS_DATA_DIR", "/elsewhere")
        .args(["--profile", "demo", "config", "path"]));
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stdout));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("--profile demo and MAILYPOPPINS_DATA_DIR=/elsewhere"),
        "{stderr}"
    );
    assert!(out.stdout.is_empty(), "nothing ran: {}", text(&out.stdout));
}

#[test]
fn mp_with_an_invalid_profile_exits_2() {
    let home = TempDir::new().unwrap();
    for (flag, env) in [(Some("../x"), None), (None, Some("much-too-long-name"))] {
        let mut cmd = mp_in(home.path());
        if let Some(name) = flag {
            cmd.args(["--profile", name]);
        }
        if let Some(name) = env {
            cmd.env("MAILYPOPPINS_PROFILE", name);
        }
        let out = run(cmd.args(["config", "path"]));
        assert_eq!(out.status.code(), Some(2), "{flag:?} {env:?}");
        let stderr = text(&out.stderr);
        assert!(stderr.contains("is not a profile name"), "{stderr}");
    }
    assert!(
        !config_base(home.path()).join("profiles").exists(),
        "a refused profile creates nothing"
    );
}

/// The pid in the runtime files under `data`.
fn daemon_pid(data: &Path) -> u32 {
    std::fs::read_to_string(data.join("runtime").join("daemon.pid"))
        .expect("the pid file")
        .trim()
        .parse()
        .expect("a pid")
}

/// The idle grace the two lifetime rows run at, in place of the minute.
const IDLE_MS: &str = "300";

/// A profile's daemon started on demand stops once its last client has been
/// gone for the idle grace (decision log: it stops once its last window
/// closes), through the orderly shutdown, so its socket is removed.
#[test]
fn a_profile_daemon_stops_after_its_last_client_leaves() {
    let home = TempDir::new().unwrap();
    let out = run(mp_in(home.path())
        .env(AUTOSTART_TIMEOUT_ENV, "8000")
        .env(IDLE_EXIT_ENV, IDLE_MS)
        .args(["--profile", "demo", "account", "list", "--daemon"]));
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let data = data_base(home.path()).join("profiles").join("demo");
    let pid = daemon_pid(&data);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while pgrep_line_for(pid).is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "the profile's daemon {pid} still runs with no client"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(
        !socket_under(&data).exists(),
        "the orderly shutdown removed the socket"
    );
}

/// The default profile's daemon is the long-lived one, and outlives its
/// clients as it always has.
#[test]
fn a_default_daemon_keeps_running_after_its_last_client_leaves() {
    let home = TempDir::new().unwrap();
    let out = run(mp_in(home.path())
        .env(AUTOSTART_TIMEOUT_ENV, "8000")
        .env(IDLE_EXIT_ENV, IDLE_MS)
        .args(["account", "list", "--daemon"]));
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let data = data_base(home.path());
    let pid = daemon_pid(&data);

    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(
        UnixStream::connect(socket_under(&data)).is_ok(),
        "the default daemon still listens five idle graces later"
    );

    let out = run(mp_in(home.path())
        .env(AUTOSTART_ENV, "0")
        .args(["daemon", "stop"]));
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while pgrep_line_for(pid).is_some() {
        assert!(std::time::Instant::now() < deadline, "{pid} still runs");
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// Two profiles, two daemons, each on its own socket, while the default
/// profile has none: the acceptance's "the default daemon keeps running
/// untouched beside it" from the other side.
#[test]
fn two_profiles_run_two_daemons_at_once() {
    let home = TempDir::new().unwrap();
    let mut pids = Vec::new();
    for profile in ["demo", "work"] {
        let out = run(mp_in(home.path()).env(AUTOSTART_TIMEOUT_ENV, "8000").args([
            "--profile",
            profile,
            "account",
            "list",
            "--daemon",
        ]));
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        assert_eq!(text(&out.stdout), "No accounts configured\n");
        let data = data_base(home.path()).join("profiles").join(profile);
        assert!(
            UnixStream::connect(socket_under(&data)).is_ok(),
            "{profile}'s daemon listens under its own data dir"
        );
        pids.push(daemon_pid(&data));
    }
    assert_ne!(pids[0], pids[1], "two daemons, not one");
    assert!(
        !socket_under(&data_base(home.path())).exists(),
        "the default profile got no daemon"
    );

    for profile in ["demo", "work"] {
        let out = run(mp_in(home.path()).env(AUTOSTART_ENV, "0").args([
            "--profile",
            profile,
            "daemon",
            "stop",
        ]));
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    }
    for pid in pids {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while pgrep_line_for(pid).is_some() {
            assert!(std::time::Instant::now() < deadline, "{pid} still runs");
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
}
