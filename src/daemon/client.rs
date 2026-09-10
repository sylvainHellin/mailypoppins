//! The client half of the daemon (P4-U2): who needs one, how one is started on
//! demand, and the single door every migrated command goes through.
//!
//! Phase 4 turns `mp` from a program that opens the store into a program that
//! asks the daemon. Three decisions have to be made once, in one place, or
//! every slice makes them again and they drift:
//!
//! 1. **Does this command need a daemon at all?** [`needs_daemon`] answers it
//!    from the command's name alone, and the answer is the no-daemon list the
//!    plan fixes: `mp daemon *`, `mp dump-keys`, `mp --help`, `mp --version`,
//!    `mp config path`. Everything else needs one, `mp config init` included:
//!    the daemon owns configuration and secrets, so an init that wrote
//!    `config.toml` behind the daemon's back would leave the running instance
//!    describing a file that no longer exists.
//! 2. **What happens when none is listening?** [`client_session`] starts one,
//!    through the same routine `mp daemon start` uses, and waits a bounded
//!    time for it. There is no fallback: a command that quietly answered from
//!    this process would let the user believe the daemon did the work.
//! 3. **Whose working directory resolves a relative path?** The client's.
//!    [`absolutise`] is applied to every user-supplied path before it crosses
//!    the socket, because the daemon is a long-lived process started from
//!    somewhere else and its own cwd carries no meaning.
//!
//! From P4-U4 on, a migrated command calls [`client_session`] and nothing else:
//! it is the seam the `MAILYPOPPINS_DAEMON_REQUIRE` hook watches, and the seam
//! P4-U15 asserts every CLI handler goes through.
//!
//! # The auto-start bound
//!
//! `client_session` tries once, and on failure spends at most
//! [`AUTOSTART_TIMEOUT`] (5 s, overridable with
//! `MAILYPOPPINS_DAEMON_AUTOSTART_TIMEOUT_MS`) on the whole start-and-connect
//! sequence: the readiness wait inside [`lifecycle::start`] first, then a
//! connect retry that backs off from 25 ms to 400 ms until the same deadline.
//! One budget rather than two, so the worst case a user waits is the number in
//! the variable and not some multiple of it.
//!
//! `MAILYPOPPINS_DAEMON_AUTOSTART=0` turns the start off and leaves the exit-4
//! diagnostic, which is what a test that wants "no daemon, and none appearing"
//! needs, and what an operator debugging a start loop wants too.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use colored::Colorize;
use log::{info, warn};

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};

use super::lifecycle::{self, EXIT_UNAVAILABLE};
use super::runtime::socket_path;

/// Opt out of on-demand starting: `0`, `false`, `no` or empty disables it,
/// anything else (and being unset) leaves it on.
///
/// A test that wants to observe "no daemon, and none appeared" sets it, and so
/// does anyone debugging a daemon that dies as fast as it is started.
pub const AUTOSTART_ENV: &str = "MAILYPOPPINS_DAEMON_AUTOSTART";

/// The whole auto-start budget in milliseconds, readiness wait and connect
/// retry together. Unset, unparseable or zero means [`AUTOSTART_TIMEOUT`].
pub const AUTOSTART_TIMEOUT_ENV: &str = "MAILYPOPPINS_DAEMON_AUTOSTART_TIMEOUT_MS";

/// Assert that this run really went through [`client_session`]: set it and a
/// command that answered from this process fails loudly instead of passing a
/// parity test it never earned.
pub const REQUIRE_ENV: &str = "MAILYPOPPINS_DAEMON_REQUIRE";

/// How long the whole start-and-connect sequence may take, absent
/// [`AUTOSTART_TIMEOUT_ENV`].
pub const AUTOSTART_TIMEOUT: Duration = Duration::from_millis(5_000);

/// How long one connect-and-handshake attempt may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// First and last gap between connect attempts after a start.
const RETRY_MIN: Duration = Duration::from_millis(25);
const RETRY_MAX: Duration = Duration::from_millis(400);

/// Set by [`client_session`] the moment a connection is handed out, read by
/// [`enforce_routing`]. Process-global because "this run talked to the daemon"
/// is a property of the run, not of any one call.
static ROUTED: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// The policy
// ---------------------------------------------------------------------------

/// Whether the command named by `command` (and `subcommand`, where the list
/// distinguishes one) has to reach the daemon.
///
/// Pure, so the no-daemon list can be asserted without running anything. The
/// names are the ones typed on the command line, kebab-case included, and
/// `mp --help` / `mp --version` are on the list under their own spellings even
/// though clap answers them before dispatch ever sees them: the list is a
/// statement about the surface, not about clap's internals, and a reader
/// checking whether `mp --version` needs a daemon must find the answer here.
///
/// `mp config init` and `mp config add-account` are deliberately **not** on the
/// list. They run as ordinary daemon clients, because the daemon owns
/// configuration and has to see the file change.
pub fn needs_daemon(command: Option<&str>, subcommand: Option<&str>) -> bool {
    match (command, subcommand) {
        // Lifecycle administration is the one family that must work when no
        // daemon exists; `mp daemon run` *is* the daemon.
        (Some("daemon"), _) => false,
        // A dump of the TUI key table, built from a compiled-in structure.
        (Some("dump-keys"), _) => false,
        // clap answers both of these and exits before dispatch.
        (Some("--help" | "-h" | "help" | "--version" | "-V"), _) => false,
        // Printing where the config file would live reads no config.
        (Some("config"), Some("path")) => false,
        _ => true,
    }
}

/// Whether on-demand starting is enabled, i.e. [`AUTOSTART_ENV`] is not an
/// explicit off.
pub fn autostart_enabled() -> bool {
    match std::env::var(AUTOSTART_ENV) {
        Ok(value) => !matches!(value.trim(), "" | "0" | "false" | "no"),
        Err(_) => true,
    }
}

/// The whole auto-start budget: [`AUTOSTART_TIMEOUT_ENV`] when it parses as a
/// nonzero number of milliseconds, [`AUTOSTART_TIMEOUT`] otherwise.
///
/// A stray or unparseable value means the default rather than a failure, the
/// same rule the daemon's other numeric hooks follow: a client may not refuse
/// to run over an environment variable it did not understand.
pub fn autostart_budget() -> Duration {
    match std::env::var(AUTOSTART_TIMEOUT_ENV) {
        Ok(value) => match value.trim().parse::<u64>() {
            Ok(ms) if ms > 0 => Duration::from_millis(ms),
            _ => AUTOSTART_TIMEOUT,
        },
        Err(_) => AUTOSTART_TIMEOUT,
    }
}

/// Whether this run was told to prove it routed through the daemon.
pub fn routing_required() -> bool {
    match std::env::var(REQUIRE_ENV) {
        Ok(value) => !matches!(value.trim(), "" | "0" | "false" | "no"),
        Err(_) => false,
    }
}

/// Whether [`client_session`] has handed out a connection in this process.
pub fn routed() -> bool {
    ROUTED.load(Ordering::SeqCst)
}

/// End the run when [`REQUIRE_ENV`] was set and nothing routed.
///
/// Called on the way out of a successful command, which is where a parity test
/// makes its assertion: a migrated command that answered from this process
/// exits nonzero saying so, instead of producing output that looks like the
/// daemon's. `what` names the command, so the failure is actionable without a
/// rerun.
///
/// It cannot catch a command that leaves through `std::process::exit` or
/// through an error, because neither returns here, and a command that failed
/// proves nothing about routing anyway. A command that returns early from the
/// dispatch owes this call on its own way out; `mp --daemon list-messages` is
/// the one that does today.
pub fn enforce_routing(what: &str) {
    if !routing_required() || routed() {
        return;
    }
    eprintln!(
        "{} {REQUIRE_ENV} is set, but `mp {what}` answered without opening a daemon session",
        "\u{2717}".red()
    );
    eprintln!("  this command still runs in process: it has not been migrated onto the daemon");
    std::process::exit(1);
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// A user-supplied path, made absolute against this process's working
/// directory.
///
/// The client is the process with a meaningful cwd; the daemon is started from
/// wherever the machine happened to start it, so a relative path that reached
/// it would resolve somewhere nobody chose. Absolutising here keeps today's
/// semantics exactly (a relative path still means "below where I am standing")
/// and makes the string that crosses the socket unambiguous.
///
/// Nothing is canonicalised: a path that does not exist yet is the normal case
/// (`mp save -o out/`), and following symlinks would answer a different
/// question than the user asked.
pub fn absolutise(path: &Path) -> PathBuf {
    match std::env::current_dir() {
        Ok(cwd) => absolutise_in(path, &cwd),
        Err(e) => {
            // A deleted cwd is the only realistic cause, and a relative path is
            // then meaningless anyway; passing it through unchanged keeps the
            // failure where it was instead of inventing a root.
            warn!("[client] cannot resolve the working directory ({e}); leaving {path:?} relative");
            path.to_path_buf()
        }
    }
}

/// The name a materialised part is written under, given the names this call has
/// already used.
///
/// The daemon materialises one part per call, into a directory of its own, and
/// never renames: two parts sent under one name come back as two handles
/// carrying that one name. The `_1` rule that turns them into two files belongs
/// where the names become paths, which is here - the same rule
/// [`crate::store::read::materialise_attachments`] applied when the client did
/// the materialising, so `mp save` writes what it always wrote.
///
/// Within one call only, deliberately: a rule that looked at what is already on
/// disk would grow a `_1` copy on every run of the same save.
pub fn unique_name(name: String, used: &[String]) -> String {
    crate::store::read::unique_in(name, used)
}

/// [`absolutise`] against an explicit base, for a path whose anchor is not the
/// cwd (a draft's own directory, say) and for tests.
pub fn absolutise_in(path: &Path, base: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

// ---------------------------------------------------------------------------
// The session
// ---------------------------------------------------------------------------

/// Connect to the local daemon, starting one if none is listening, or end the
/// run with exit 4.
///
/// The single entry point for every command that reaches the daemon. It never
/// returns a failure: a caller that has to answer from the daemon has nothing
/// to do with one, and a `Result` here would invite exactly the silent
/// fallback the migration forbids.
pub async fn client_session() -> Connection {
    let socket = socket_path();

    if let Ok(connection) = connect(&socket).await {
        ROUTED.store(true, Ordering::SeqCst);
        return connection;
    }

    if !autostart_enabled() {
        unavailable(
            &format!("none is listening and {AUTOSTART_ENV} turned on-demand starting off"),
            &socket,
        );
    }

    let budget = autostart_budget();
    let deadline = Instant::now() + budget;
    if let Err(e) = autostart(budget).await {
        unavailable(&format!("{e:#}"), &socket);
    }

    let mut gap = RETRY_MIN;
    loop {
        match connect(&socket).await {
            Ok(connection) => {
                ROUTED.store(true, Ordering::SeqCst);
                return connection;
            }
            Err(e) => {
                if Instant::now() >= deadline {
                    unavailable(
                        &format!(
                            "one was started but did not answer within {} ms: {e}",
                            budget.as_millis()
                        ),
                        &socket,
                    );
                }
                tokio::time::sleep(gap).await;
                gap = (gap * 2).min(RETRY_MAX);
            }
        }
    }
}

/// One connect plus `initialize`, under [`CONNECT_TIMEOUT`].
async fn connect(socket: &Path) -> Result<Connection, ClientError> {
    let handshake = async {
        let mut connection = Connection::connect(socket).await?;
        connection
            .initialize(
                ClientInfo {
                    kind: ClientKind::Cli,
                    app_version: env!("CARGO_PKG_VERSION").to_string(),
                },
                Identity {
                    data_dir: crate::config::mailypoppins_data_dir(),
                    config_dir: crate::config::config_dir(),
                },
                &[],
                &[],
            )
            .await?;
        Ok::<Connection, ClientError>(connection)
    };
    match tokio::time::timeout(CONNECT_TIMEOUT, handshake).await {
        Ok(result) => result,
        Err(_) => Err(ClientError::Protocol(format!(
            "it did not complete the handshake within {}s",
            CONNECT_TIMEOUT.as_secs()
        ))),
    }
}

/// Start a daemon on demand, through the same routine `mp daemon start` uses.
///
/// Sharing it is the point: that routine already takes the start lock so two
/// clients racing produce one daemon, sweeps a socket a crash left behind,
/// spawns `std::env::current_exe()` in its own session with its stdio in the
/// daemon log, and fails early when the child dies instead of waiting out the
/// bound. A second implementation here would be a second set of those bugs.
async fn autostart(budget: Duration) -> Result<()> {
    info!(
        "[client] no daemon at the socket; starting one on demand (bound {} ms)",
        budget.as_millis()
    );
    match lifecycle::start(budget).await? {
        0 => Ok(()),
        code => bail!("`mp daemon run` could not be started (exit {code})"),
    }
}

/// The exit-4 diagnostic: why, where the socket is, where the log is, and the
/// exact command to run by hand.
pub fn unavailable(why: &str, socket: &Path) -> ! {
    eprintln!(
        "{} no mailypoppins daemon could serve this command: {why}",
        "\u{2717}".red()
    );
    eprintln!("  socket:     {}", socket.display());
    eprintln!("  daemon log: {}", lifecycle::daemon_log_path().display());
    eprintln!("  start one:  mp daemon run");
    std::process::exit(EXIT_UNAVAILABLE);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The no-daemon list, exactly as plan section 3.6 (P4-U2) fixes it.
    #[test]
    fn the_no_daemon_list_is_the_five_entries_the_plan_names() {
        for (command, sub) in [
            ("daemon", Some("run")),
            ("daemon", Some("start")),
            ("daemon", Some("status")),
            ("daemon", Some("stop")),
            ("daemon", Some("restart")),
            ("dump-keys", None),
            ("--help", None),
            ("--version", None),
            ("config", Some("path")),
        ] {
            assert!(
                !needs_daemon(Some(command), sub),
                "`mp {command} {sub:?}` is on the no-daemon list"
            );
        }
    }

    /// The one entry the plan calls out by name as *not* being on the list:
    /// the daemon owns configuration, so an init it did not see would leave it
    /// describing a file that changed underneath it.
    #[test]
    fn config_init_and_add_account_are_not_on_the_no_daemon_list() {
        assert!(needs_daemon(Some("config"), Some("init")));
        assert!(needs_daemon(Some("config"), Some("add-account")));
        assert!(needs_daemon(Some("config"), Some("show")));
    }

    #[test]
    fn every_other_command_needs_a_daemon() {
        for command in [
            "send",
            "list",
            "new",
            "sync",
            "watch",
            "save",
            "show",
            "search",
            "archive",
            "delete",
            "outbox",
            "store",
            "contacts",
            "calendar",
            "account",
            "cutover",
            "dump-mailbox",
        ] {
            assert!(
                needs_daemon(Some(command), None),
                "`mp {command}` is not on the no-daemon list, so it needs a daemon"
            );
        }
        // A bare `mp` is the TUI or a dry-run preview; both need the engine.
        assert!(needs_daemon(None, None));
    }

    #[test]
    fn absolutisation_keeps_an_absolute_path_and_anchors_a_relative_one() {
        assert_eq!(
            absolutise_in(Path::new("/tmp/out"), Path::new("/somewhere/else")),
            PathBuf::from("/tmp/out"),
            "an absolute path is already unambiguous"
        );
        assert_eq!(
            absolutise_in(Path::new("out"), Path::new("/home/user/work")),
            PathBuf::from("/home/user/work/out")
        );
        assert_eq!(
            absolutise_in(Path::new("."), Path::new("/home/user/work")),
            PathBuf::from("/home/user/work/."),
            "the default `mp save` destination, anchored but not normalised: \
             canonicalising would resolve symlinks the user did not ask about"
        );
    }
}
