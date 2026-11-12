//! `mp daemon install-service | uninstall-service` (P6-U6, `LIF-06`).
//!
//! Two local commands that write one file and call one service manager. No
//! daemon has to be running and no wire method is added: asking a running
//! daemon to arrange for a daemon to run would be circular, and the file this
//! writes is the thing that starts one. A daemon that is running is handed
//! over to the service the install enables (PERSO-219, see [`install`]).
//!
//! ## What is written, and where
//!
//! | target | path | template |
//! |---|---|---|
//! | linux | `$XDG_CONFIG_HOME/systemd/user/mailypoppins.service` | [`UNIT_TEMPLATE`] |
//! | darwin | `$HOME/Library/LaunchAgents/dev.mailypoppins.daemon.plist` | [`PLIST_TEMPLATE`] |
//!
//! `$XDG_CONFIG_HOME` falls back to `$HOME/.config` when unset, empty or
//! relative, which is XDG's own rule and systemd's. Both files are written 0644 with their parent directories
//! created as needed, and neither holds a secret.
//!
//! All three substituted values are paths a user chose, so the templates quote
//! every placeholder - a double-quoted systemd value, a plist `<string>` - and
//! [`escape`] makes the value unable to leave the construct it sits in. A
//! `$HOME` or a `MAILYPOPPINS_DATA_DIR` with a space in it would otherwise
//! split an `ExecStart` into two arguments and truncate an `Environment=` at
//! the space, and an `&` in a path would make the plist unparseable.
//!
//! The two templates are byte-for-byte copies of the fixtures
//! `tests/daemon_service.rs` pins, `src/daemon/templates/`, with three
//! placeholders: `{{MP}}` is [`std::env::current_exe`] as it is, or the `mp`
//! on `PATH` when that is the same file (see [`service_exe`]), `{{DATA_DIR}}` and
//! `{{CONFIG_DIR}}` the two directories the installing `mp` resolved,
//! canonicalised, i.e. exactly the strings `mp daemon status --json` reports.
//! Baking those two in is the point of installing the service at all: a
//! login-started daemon inherits the session manager's environment and not the
//! shell's, so a user whose `MAILYPOPPINS_DATA_DIR` is exported from a shell rc
//! file would otherwise get a daemon serving a different tree than the one his
//! `mp` talks to. [`the_templates_are_the_committed_fixtures`] keeps the copies
//! honest and [`the_stop_timeout_follows_the_shutdown_grace`] keeps their
//! `TimeoutStopSec` / `ExitTimeOut` tied to [`DEFAULT_GRACE_SECS`], which is
//! the tie that stops a service manager `SIGKILL`ing a daemon in the middle of
//! the eight shutdown steps. That grace is
//! [`DEFAULT_GRACE_SECS`](super::shutdown::DEFAULT_GRACE_SECS) plus five
//! seconds: long enough for the eight steps to finish once the grace expires,
//! short enough that a stuck daemon does not hold up a logout.
//!
//! ## The two environment hooks
//!
//! - [`DRY_RUN_ENV`] renders, writes and removes the file exactly as usual and
//!   runs no `systemctl` and no `launchctl`, printing the lines it would have
//!   run plus one saying nothing was run. This is what keeps the contract suite
//!   off the developer's own user session.
//! - [`OS_ENV`] selects the half; unset means this build's target OS. It exists
//!   because the launchd half cannot be smoke-tested on the machine this
//!   project is developed on, so without it the plist would be reachable from
//!   no test at all.
//!
//! ## What a failure costs
//!
//! The file is what these commands own; enabling it is what they asked the
//! session manager for. A `systemctl` that runs and fails is exit 1 with the
//! unit left on disk, because removing the half that worked would leave the
//! user with neither. No `systemctl` on `PATH` at all is exit 0 with the lines
//! printed to run by hand: that is a container, a minimal image, or a session
//! that is not systemd's, and refusing to write the file there would make the
//! command useless exactly where a user would copy the unit somewhere else
//! himself.
//!
//! ## Starting through the service (PERSO-109)
//!
//! [`start_route`] is what `mp daemon start`, `mp daemon restart` and a
//! client's on-demand start (PERSO-219) ask before they bring a daemon up. When the installed file bakes this run's
//! data directory and runs this executable, its manager is on `PATH`, and on
//! Linux a systemd user manager is reachable (`$XDG_RUNTIME_DIR/systemd/private`
//! is a socket), the answer is the manager's start command (`systemctl --user
//! start`, or `launchctl kickstart` / `bootstrap`), so the daemon is the
//! service's own process instead of a detached one beside a dead unit. A
//! restart asks for the manager's restart instead (`systemctl --user restart`,
//! `launchctl kickstart -k`), because the manager may still count the service
//! running for a moment after the socket stop's daemon exited, and a start in
//! that moment is a no-op that leaves the service dead once it notices.
//! Anything else is the detached start, with a note when a service was there
//! and passed over. The decision is [`decide_start`], a pure function of what
//! the probe found, and the only command the probe runs is `launchctl print`,
//! on macOS, once a matching agent is installed.

use std::ffi::OsStr;
use std::fs::{self, Permissions};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use colored::Colorize;
use log::info;

use super::lifecycle::env_flag;

/// Render and write as usual, but run no service-manager command.
pub const DRY_RUN_ENV: &str = "MAILYPOPPINS_DAEMON_SERVICE_DRY_RUN";

/// `linux` or `darwin`; unset means this build's target OS.
pub const OS_ENV: &str = "MAILYPOPPINS_DAEMON_SERVICE_OS";

/// The systemd unit's name, which every `systemctl` line also carries.
const UNIT_NAME: &str = "mailypoppins.service";

/// The launchd label: the plist's file stem and the last component of the
/// `bootout` target.
const LAUNCHD_LABEL: &str = "dev.mailypoppins.daemon";

/// How much longer than the shutdown grace the service manager waits before it
/// resorts to `SIGKILL`. The templates carry the sum as a literal, and
/// [`the_stop_timeout_follows_the_shutdown_grace`] is what ties it back here.
#[cfg(test)]
const STOP_MARGIN_SECS: u64 = 5;

/// The systemd user unit, byte-identical to `tests/fixtures/service/`.
const UNIT_TEMPLATE: &str = include_str!("templates/mailypoppins.service");

/// The launchd user agent, byte-identical to `tests/fixtures/service/`.
const PLIST_TEMPLATE: &str = include_str!("templates/dev.mailypoppins.daemon.plist");

const EXIT_OK: i32 = 0;
const EXIT_ERROR: i32 = 1;

/// Which platform's service manager a run targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Linux,
    Darwin,
}

/// One service-manager invocation.
#[derive(Debug)]
struct Invocation {
    program: &'static str,
    args: Vec<String>,
}

impl Invocation {
    fn new(program: &'static str, args: &[String]) -> Self {
        Self {
            program,
            args: args.to_vec(),
        }
    }

    /// `systemctl --user daemon-reload`, as printed and as run.
    fn display(&self) -> String {
        format!("{} {}", self.program, self.args.join(" "))
    }

    /// The same, indented two spaces, which is how every detail line under a
    /// `✓` reads.
    fn line(&self) -> String {
        format!("  {}", self.display())
    }
}

/// Whether the service-manager commands are run, skipped, or unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    DryRun,
    NoManager,
    Run,
}

// ---------------------------------------------------------------------------
// install-service
// ---------------------------------------------------------------------------

/// Write the unit for this platform and ask the session manager to enable it.
///
/// A daemon already answering on the socket is handed over to the service
/// (PERSO-219) whenever the service manager is run and the service can start
/// this daemon ([`start_route`]): the service is registered without being
/// started, the daemon is stopped over its socket, and the manager's restart
/// brings the service up, which is `mp daemon restart`'s stop and start. An
/// `enable --now` beside it would start a daemon that cannot bind the socket
/// the running one holds, so the unit would crash-loop until that one
/// stopped. A registration that fails exits 1 with the running daemon left
/// alone.
pub async fn install(force: bool, check: bool) -> Result<i32> {
    let target = resolve_target(std::env::var(OS_ENV).ok().as_deref())?;
    let path = service_path(target);

    if check {
        return Ok(report_check(&path));
    }

    let desired = render(target)?;
    let existing = fs::read_to_string(&path).ok();
    let header = match existing {
        Some(current) if current == desired => {
            format!("{} {} is already installed", tick(), path.display())
        }
        Some(_) if !force => bail!(
            "{} differs from the service this version installs; re-run with --force to replace it",
            path.display()
        ),
        _ => {
            write_service(target, &path, &desired)?;
            info!("[service] wrote {}", path.display());
            format!("{} wrote {}", tick(), path.display())
        }
    };

    let commands = install_commands(target, &path);
    let mode = mode_for(commands[0].program);
    if mode == Mode::Run {
        if let Some(pid) = super::lifecycle::running_pid().await {
            if let StartRoute::Supervised(plan) = start_route(StartKind::Restart)? {
                return hand_over(target, header, &pid, plan).await;
            }
        }
    }
    let mut lines = vec![header];
    lines.extend(commands.iter().map(Invocation::line));
    lines.extend(note(mode, commands[0].program, "enable"));

    let outcome = match mode {
        Mode::Run => run_all(&commands, ManagerOutput::Terminal),
        _ => Ok(()),
    };
    print_lines(&lines);
    outcome?;
    Ok(EXIT_OK)
}

/// Register the service without starting it, then stop the running daemon
/// over its socket and start the service through `plan`.
async fn hand_over(
    target: Target,
    header: String,
    pid: &str,
    plan: SupervisedStart,
) -> Result<i32> {
    let commands = register_commands(target);
    let mut lines = vec![header];
    lines.extend(commands.iter().map(Invocation::line));
    let outcome = run_all(&commands, ManagerOutput::Terminal);
    print_lines(&lines);
    outcome?;
    info!("[service] handing the running daemon (pid {pid}) over to the service");
    println!("  the running daemon (pid {pid}) is handed over to the service");
    super::lifecycle::hand_over(StartRoute::Supervised(plan)).await
}

/// `--check`: report, write nothing, and exit 1 when there is nothing there.
///
/// The `✗` goes to stdout rather than to stderr, because this is a report the
/// command was asked for and not a failure of it; the exit code is what a
/// script branches on.
fn report_check(path: &Path) -> i32 {
    if path.exists() {
        println!("{} service installed at {}", tick(), path.display());
        return EXIT_OK;
    }
    println!("{} no service installed", cross());
    println!("  install one: mp daemon install-service");
    EXIT_ERROR
}

// ---------------------------------------------------------------------------
// uninstall-service
// ---------------------------------------------------------------------------

/// Disable the service, remove the file, and tell the session manager.
pub fn uninstall() -> Result<i32> {
    let target = resolve_target(std::env::var(OS_ENV).ok().as_deref())?;
    let path = service_path(target);

    if !path.exists() {
        // Not an error: the user asked for a machine with no login service,
        // and that is what the machine has. Nothing is run either, so a
        // session that never had the unit is not asked to disable it.
        println!("{} no service installed", tick());
        return Ok(EXIT_OK);
    }

    let commands = uninstall_commands(target, &path);
    let mode = mode_for(commands[0].program);
    let mut lines = vec![format!("{} removed {}", tick(), path.display())];
    lines.extend(commands.iter().map(Invocation::line));
    lines.extend(note(mode, commands[0].program, "disable"));

    // The disable runs while the unit file is still readable and the reload
    // only once it is gone, which is why the removal sits between the two
    // rather than before them.
    let outcome = (|| -> Result<()> {
        if mode == Mode::Run {
            run_all(&commands[..1], ManagerOutput::Terminal)?;
        }
        remove_service(&path)?;
        if mode == Mode::Run {
            run_all(&commands[1..], ManagerOutput::Terminal)?;
        }
        Ok(())
    })();
    print_lines(&lines);
    outcome?;
    info!("[service] removed {}", path.display());
    Ok(EXIT_OK)
}

// ---------------------------------------------------------------------------
// The target, the path and the content
// ---------------------------------------------------------------------------

/// Read [`OS_ENV`], or fall back to this build's target OS.
///
/// An unrecognised value is an error rather than a silent default: a user who
/// misspelled the variable asked for something this build cannot do, and
/// installing the other platform's file would be the worst available answer.
fn resolve_target(value: Option<&str>) -> Result<Target> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(if cfg!(target_os = "macos") {
            Target::Darwin
        } else {
            Target::Linux
        }),
        Some("linux") => Ok(Target::Linux),
        Some("darwin") => Ok(Target::Darwin),
        Some(other) => bail!("{OS_ENV}={other} is not a platform this build knows: it takes `linux` (a systemd user unit) or `darwin` (a launchd user agent)"),
    }
}

/// Where this target's service file lives.
fn service_path(target: Target) -> PathBuf {
    match target {
        Target::Linux => xdg_config_home()
            .join("systemd")
            .join("user")
            .join(UNIT_NAME),
        Target::Darwin => home_dir()
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{LAUNCHD_LABEL}.plist")),
    }
}

/// `$XDG_CONFIG_HOME`, falling back to `$HOME/.config`.
fn xdg_config_home() -> PathBuf {
    xdg_config_home_from(std::env::var_os("XDG_CONFIG_HOME"), home_dir())
}

/// The rule itself: the XDG spec says a relative value is invalid and is to
/// be ignored, and systemd ignores it too, so honouring one would write the
/// unit where the user manager never looks.
fn xdg_config_home_from(value: Option<std::ffi::OsString>, home: PathBuf) -> PathBuf {
    match value.map(PathBuf::from) {
        Some(path) if path.is_absolute() => path,
        _ => home.join(".config"),
    }
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// This target's template with the three paths substituted.
fn render(target: Target) -> Result<String> {
    let exe = std::env::current_exe().context("resolving this executable")?;
    let exe = service_exe(exe, std::env::var_os("PATH"));
    Ok(render_template(
        target,
        &exe.display().to_string(),
        &canonical(&crate::config::mailypoppins_data_dir()),
        &canonical(&crate::config::config_dir()),
    ))
}

/// The substitution itself, without an environment to read.
///
/// Every value goes through [`escape`] on its way in, because all three are
/// paths a user chose and a path may hold a space, an `&` or a quote.
fn render_template(target: Target, mp: &str, data_dir: &str, config_dir: &str) -> String {
    template(target)
        .replace("{{MP}}", &escape(target, mp))
        .replace("{{DATA_DIR}}", &escape(target, data_dir))
        .replace("{{CONFIG_DIR}}", &escape(target, config_dir))
}

/// One substituted value, made safe for the file it is going into.
///
/// The templates put every placeholder inside a double-quoted systemd value or
/// inside a plist `<string>`, so the quoting is the template's and the
/// escaping is this: what a value must not be able to do is *leave* the
/// construct it sits in.
///
/// - **linux**: `\` and `"` are the two characters systemd reads specially
///   inside a double-quoted value (`systemd.syntax(7)`), so both are
///   backslash-escaped. A `$HOME` with a space in it then renders as one
///   argument instead of two, and an `Environment=` assignment with a space
///   in its value is no longer silently truncated at the space.
/// - **darwin**: `&`, `<`, `>` and `"` are XML's, so all four become entities.
///   A path holding an `&` otherwise makes the whole plist unparseable, which
///   is a `launchctl bootstrap` that fails at login rather than at install.
fn escape(target: Target, value: &str) -> String {
    match target {
        Target::Linux => value.replace('\\', "\\\\").replace('"', "\\\""),
        Target::Darwin => value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;"),
    }
}

fn template(target: Target) -> &'static str {
    match target {
        Target::Linux => UNIT_TEMPLATE,
        Target::Darwin => PLIST_TEMPLATE,
    }
}

/// An absolute, symlink-resolved path as a string, which is what the daemon's
/// own runtime metadata carries and therefore what the service file must.
/// The binary the service runs: this executable as the OS named it, never
/// canonicalised, because a Homebrew install's canonical path is a
/// version-stamped Cellar directory that the next `brew upgrade` deletes.
/// When the first `mp` on `PATH` is this very file (its `bin/mp` symlink,
/// say), that stable entry is baked instead.
fn service_exe(exe: PathBuf, path_var: Option<std::ffi::OsString>) -> PathBuf {
    let on_path = path_var.as_deref().and_then(|dirs| {
        std::env::split_paths(dirs)
            .map(|dir| dir.join("mp"))
            .find(|candidate| is_executable(candidate))
    });
    match on_path {
        Some(candidate) if candidate.is_absolute() && same_file(&candidate, &exe) => candidate,
        _ => exe,
    }
}

/// A regular file (after symlinks) this user may run.
fn is_executable(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

/// Whether two paths reach one file, symlinks followed.
fn same_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (fs::metadata(a), fs::metadata(b)) {
            (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        matches!(
            (fs::canonicalize(a), fs::canonicalize(b)),
            (Ok(a), Ok(b)) if a == b
        )
    }
}

fn canonical(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

/// Write the service file 0644, creating what it needs.
///
/// The darwin half also creates `<data_dir>/logs`, because launchd refuses a
/// job whose `StandardOutPath` names a directory that does not exist. It is
/// the directory `mp daemon start` already points a detached daemon's stdio at.
fn write_service(target: Target, path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    if target == Target::Darwin {
        let logs = crate::config::logs_dir();
        crate::config::create_private_dir_all(&logs)
            .with_context(|| format!("creating the log directory {}", logs.display()))?;
    }
    fs::write(path, content).with_context(|| format!("writing {}", path.display()))?;
    fs::set_permissions(path, Permissions::from_mode(0o644))
        .with_context(|| format!("setting mode 0644 on {}", path.display()))?;
    Ok(())
}

fn remove_service(path: &Path) -> Result<()> {
    fs::remove_file(path).with_context(|| format!("removing {}", path.display()))
}

// ---------------------------------------------------------------------------
// The service manager
// ---------------------------------------------------------------------------

/// Reload, then enable: a unit systemd has not read yet cannot be enabled.
fn install_commands(target: Target, path: &Path) -> Vec<Invocation> {
    match target {
        Target::Linux => vec![
            Invocation::new("systemctl", &strings(&["--user", "daemon-reload"])),
            Invocation::new(
                "systemctl",
                &strings(&["--user", "enable", "--now", UNIT_NAME]),
            ),
        ],
        Target::Darwin => vec![Invocation::new(
            "launchctl",
            &strings(&["bootstrap", &gui_domain(), &path.display().to_string()]),
        )],
    }
}

/// What registers the service without starting it, for a handover whose start
/// is the manager's restart: reload, then enable without `--now`. Nothing on
/// macOS, where `bootstrap` both loads and starts the agent and is therefore
/// the start itself.
fn register_commands(target: Target) -> Vec<Invocation> {
    match target {
        Target::Linux => vec![
            Invocation::new("systemctl", &strings(&["--user", "daemon-reload"])),
            Invocation::new("systemctl", &strings(&["--user", "enable", UNIT_NAME])),
        ],
        Target::Darwin => Vec::new(),
    }
}

/// Disable, then reload: reloading before the unit file is gone re-reads it.
fn uninstall_commands(target: Target, _path: &Path) -> Vec<Invocation> {
    match target {
        Target::Linux => vec![
            Invocation::new(
                "systemctl",
                &strings(&["--user", "disable", "--now", UNIT_NAME]),
            ),
            Invocation::new("systemctl", &strings(&["--user", "daemon-reload"])),
        ],
        // `bootout` takes the service target, not the path, so it works after
        // the plist is gone as well as before.
        Target::Darwin => vec![Invocation::new(
            "launchctl",
            &strings(&["bootout", &format!("{}/{LAUNCHD_LABEL}", gui_domain())]),
        )],
    }
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_string()).collect()
}

/// The user's own GUI domain, `gui/<uid>`.
fn gui_domain() -> String {
    // SAFETY: `getuid` takes no argument, cannot fail and touches no memory.
    let uid = unsafe { libc::getuid() };
    format!("gui/{uid}")
}

fn mode_for(program: &str) -> Mode {
    if env_flag(DRY_RUN_ENV) {
        Mode::DryRun
    } else if on_path(program) {
        Mode::Run
    } else {
        Mode::NoManager
    }
}

/// The one line appended under the command block, when there is one.
fn note(mode: Mode, program: &str, verb: &str) -> Option<String> {
    match mode {
        Mode::DryRun => Some(format!("  dry run: {DRY_RUN_ENV} is set, nothing was run")),
        Mode::NoManager => Some(format!(
            "  {program} is not on PATH; run the lines above to {verb} the service"
        )),
        Mode::Run => None,
    }
}

/// Where a service-manager command's own input and output go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ManagerOutput {
    /// The terminal's, inherited: `install-service`, `mp daemon start` and
    /// `restart` show what the manager says, a failure's reason included.
    Terminal,
    /// None: stdin is null and stdout and stderr are captured, so an
    /// on-demand start inside another command prints nothing into that
    /// command's output or onto a TUI's raw-mode screen, and a failure's
    /// error carries what the manager said instead.
    Captured,
}

fn run_all(commands: &[Invocation], output: ManagerOutput) -> Result<()> {
    for command in commands {
        run_one(command, output)?;
    }
    Ok(())
}

fn run_one(command: &Invocation, output: ManagerOutput) -> Result<()> {
    let mut process = Command::new(command.program);
    process.args(&command.args);
    let (status, said) = match output {
        ManagerOutput::Terminal => (process.status(), String::new()),
        ManagerOutput::Captured => match process.stdin(std::process::Stdio::null()).output() {
            Ok(out) => (Ok(out.status), captured_text(&out.stderr, &out.stdout)),
            Err(e) => (Err(e), String::new()),
        },
    };
    let status = status.with_context(|| format!("running `{}`", command.display()))?;
    if !status.success() {
        let said = if said.is_empty() {
            said
        } else {
            format!(": {said}")
        };
        match status.code() {
            Some(code) => bail!("`{}` exited with {code}{said}", command.display()),
            None => bail!("`{}` was killed by a signal{said}", command.display()),
        }
    }
    Ok(())
}

/// What a captured command printed, stderr first, as one line: the error it
/// goes into is a single log line.
fn captured_text(stderr: &[u8], stdout: &[u8]) -> String {
    [stderr, stdout]
        .iter()
        .flat_map(|stream| {
            String::from_utf8_lossy(stream)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Whether `name` resolves to an executable on this process's `PATH`.
fn on_path(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            let candidate = dir.join(name);
            fs::metadata(&candidate)
                .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        })
    })
}

// ---------------------------------------------------------------------------
// Starting through the service manager (PERSO-109)
// ---------------------------------------------------------------------------

/// Which command brings the daemon up: `mp daemon start`'s or the start half
/// of `mp daemon restart`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartKind {
    /// Nothing answers on the socket, and the service manager is asked to
    /// start the service, which it does not do to a service it counts running.
    Start,
    /// The daemon was just stopped over its socket. Its process has exited,
    /// but the manager may not have processed that exit yet and still count
    /// the service running, so a start would be a no-op and the service would
    /// then end dead (PERSO-109). The manager's restart stops what it counts
    /// running, which waits for that exit, and starts the service in one job.
    Restart,
}

/// How `mp daemon start` and `mp daemon restart` bring a daemon up.
#[derive(Debug)]
pub(crate) enum StartRoute {
    /// Spawn a detached `mp daemon run`, which is what a machine with no
    /// service for this data directory gets. The note, when there is one,
    /// says why an installed service was passed over.
    Detached { note: Option<String> },
    /// Ask the service manager to start the installed service, so the daemon
    /// is the unit's own process and the unit does not read as dead.
    Supervised(SupervisedStart),
}

/// The service-manager half of a start: what to run, and where to look when
/// the service started but its daemon never answered.
#[derive(Debug)]
pub(crate) struct SupervisedStart {
    target: Target,
    commands: Vec<Invocation>,
}

impl SupervisedStart {
    /// Run the commands in order, stopping at the first that fails, with
    /// their output where `output` says.
    pub(crate) fn run(&self, output: ManagerOutput) -> Result<()> {
        run_all(&self.commands, output).with_context(|| {
            format!(
                "starting the daemon through {} (see `{}`)",
                service_name(self.target),
                self.status_hint()
            )
        })
    }

    /// The command lines, indented as every detail line under a `✓` is.
    pub(crate) fn lines(&self) -> Vec<String> {
        self.commands.iter().map(Invocation::line).collect()
    }

    /// The command that shows what the service manager knows of the service.
    pub(crate) fn status_hint(&self) -> String {
        match self.target {
            Target::Linux => format!("systemctl --user status {UNIT_NAME}"),
            Target::Darwin => format!("launchctl print {}/{LAUNCHD_LABEL}", gui_domain()),
        }
    }
}

/// How the installed service file relates to this `mp`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Installed {
    /// No service file.
    Absent,
    /// A service file for another data directory, or one this build cannot
    /// read a data directory or an executable out of. Its daemon would not be
    /// the one this `mp` talks to, which is what keeps a test run with a
    /// temporary data directory off the user's real service.
    Foreign,
    /// A service for this data directory that runs another executable.
    OtherExe(String),
    /// A service for this data directory that runs this executable.
    This,
}

/// Probe the service file and the service manager, then decide.
///
/// Nothing is run unless a service file for this data directory that runs
/// this executable is installed, and then only `launchctl print` on macOS:
/// every other machine, a build host and a test run included, decides from
/// the file system alone.
pub(crate) fn start_route(kind: StartKind) -> Result<StartRoute> {
    let target = resolve_target(std::env::var(OS_ENV).ok().as_deref())?;
    let path = service_path(target);
    let installed = match fs::read_to_string(&path) {
        Err(_) => Installed::Absent,
        Ok(content) => {
            let exe = std::env::current_exe().context("resolving this executable")?;
            classify(
                target,
                &content,
                &crate::config::mailypoppins_data_dir(),
                &exe,
            )
        }
    };
    let manager_on_path = installed == Installed::This && on_path(manager(target));
    let loaded = manager_on_path && target == Target::Darwin && launchd_loaded();
    let unreachable = if manager_on_path && target == Target::Linux {
        user_manager_unreachable(std::env::var_os("XDG_RUNTIME_DIR").as_deref())
    } else {
        None
    };
    Ok(decide_start(
        target,
        &path,
        kind,
        installed,
        manager_on_path,
        unreachable,
        loaded,
    ))
}

/// Why no systemd user manager answers this process, or `None` when one does.
///
/// A `systemctl` binary on `PATH` proves nothing: WSL without systemd, a
/// container, or a cron job with no session has the binary and no manager, and
/// `systemctl --user` then fails after a restart has already stopped the
/// daemon. The manager listens on `$XDG_RUNTIME_DIR/systemd/private`, so that
/// socket existing is the check; nothing is run.
fn user_manager_unreachable(runtime_dir: Option<&OsStr>) -> Option<String> {
    let Some(dir) = runtime_dir.filter(|dir| !dir.is_empty()) else {
        return Some("XDG_RUNTIME_DIR is not set".to_string());
    };
    let socket = Path::new(dir).join("systemd").join("private");
    match fs::metadata(&socket) {
        Ok(meta) if meta.file_type().is_socket() => None,
        Ok(_) => Some(format!("{} is not a socket", socket.display())),
        Err(_) => Some(format!("{} does not exist", socket.display())),
    }
}

/// The decision itself, with every fact it reads passed in.
///
/// A service is used only when it is this data directory's and runs this
/// executable, because `mp daemon restart` promises this executable's daemon
/// and a development build restarted from `target/` would otherwise come back
/// as the installed one. Passing a service over is said on stderr, so a user
/// who expected the service learns why it stayed stopped.
///
/// `unreachable` is [`user_manager_unreachable`]'s answer on Linux and `None`
/// on macOS, where `launchctl` reaches launchd whenever it runs.
fn decide_start(
    target: Target,
    path: &Path,
    kind: StartKind,
    installed: Installed,
    manager_on_path: bool,
    unreachable: Option<String>,
    loaded: bool,
) -> StartRoute {
    let name = service_name(target);
    match installed {
        Installed::Absent | Installed::Foreign => StartRoute::Detached { note: None },
        Installed::OtherExe(exe) => StartRoute::Detached {
            note: Some(format!(
                "{name} runs {exe}, not this executable, so this daemon starts outside the service"
            )),
        },
        Installed::This if !manager_on_path => StartRoute::Detached {
            note: Some(format!(
                "{name} is installed but {} is not on PATH, so this daemon starts outside the service",
                manager(target)
            )),
        },
        Installed::This if unreachable.is_some() => StartRoute::Detached {
            note: Some(format!(
                "{name} is installed but no {} user manager is reachable ({}), so this daemon starts outside the service",
                manager_kind(target),
                unreachable.unwrap_or_default()
            )),
        },
        Installed::This => StartRoute::Supervised(SupervisedStart {
            target,
            commands: start_commands(target, path, kind, loaded),
        }),
    }
}

/// `systemctl --user start`; on macOS `kickstart` for a loaded agent and
/// `bootstrap` for one launchd does not know, since `kickstart` refuses a
/// label that is not loaded and `bootstrap` one that is.
///
/// A [`StartKind::Restart`] is `systemctl --user restart` and `kickstart -k`
/// instead: both stop the instance the manager still counts running, which
/// for a daemon that already exited only waits for the manager to notice, and
/// then start the service, where a plain start would see it running and do
/// nothing. An agent launchd has not loaded runs nothing it could count, so
/// `bootstrap` serves both.
fn start_commands(target: Target, path: &Path, kind: StartKind, loaded: bool) -> Vec<Invocation> {
    let label = format!("{}/{LAUNCHD_LABEL}", gui_domain());
    match (target, kind) {
        (Target::Linux, StartKind::Start) => vec![Invocation::new(
            "systemctl",
            &strings(&["--user", "start", UNIT_NAME]),
        )],
        (Target::Linux, StartKind::Restart) => vec![Invocation::new(
            "systemctl",
            &strings(&["--user", "restart", UNIT_NAME]),
        )],
        (Target::Darwin, StartKind::Start) if loaded => {
            vec![Invocation::new(
                "launchctl",
                &strings(&["kickstart", &label]),
            )]
        }
        (Target::Darwin, StartKind::Restart) if loaded => vec![Invocation::new(
            "launchctl",
            &strings(&["kickstart", "-k", &label]),
        )],
        (Target::Darwin, _) => vec![Invocation::new(
            "launchctl",
            &strings(&["bootstrap", &gui_domain(), &path.display().to_string()]),
        )],
    }
}

fn service_name(target: Target) -> &'static str {
    match target {
        Target::Linux => UNIT_NAME,
        Target::Darwin => LAUNCHD_LABEL,
    }
}

fn manager(target: Target) -> &'static str {
    match target {
        Target::Linux => "systemctl",
        Target::Darwin => "launchctl",
    }
}

fn manager_kind(target: Target) -> &'static str {
    match target {
        Target::Linux => "systemd",
        Target::Darwin => "launchd",
    }
}

/// Whether launchd has the agent loaded in this user's GUI domain.
fn launchd_loaded() -> bool {
    Command::new("launchctl")
        .args(["print", &format!("{}/{LAUNCHD_LABEL}", gui_domain())])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Compare an installed service file with this `mp` and its data directory.
fn classify(target: Target, content: &str, data_dir: &Path, exe: &Path) -> Installed {
    let (Some(baked_exe), Some(baked_dir)) = baked(target, content) else {
        return Installed::Foreign;
    };
    if canonical(Path::new(&baked_dir)) != canonical(data_dir) {
        return Installed::Foreign;
    }
    if same_file(Path::new(&baked_exe), exe) {
        Installed::This
    } else {
        Installed::OtherExe(baked_exe)
    }
}

/// The executable and the data directory a service file bakes, unescaped.
fn baked(target: Target, content: &str) -> (Option<String>, Option<String>) {
    match target {
        Target::Linux => {
            let exe = content
                .lines()
                .find_map(|line| line.trim().strip_prefix("ExecStart="))
                .and_then(first_systemd_word);
            let data_dir = content
                .lines()
                .filter_map(|line| line.trim().strip_prefix("Environment="))
                .filter_map(first_systemd_word)
                .find_map(|assignment| {
                    assignment
                        .strip_prefix("MAILYPOPPINS_DATA_DIR=")
                        .map(str::to_string)
                });
            (exe, data_dir)
        }
        Target::Darwin => (
            plist_string_after(content, "<key>ProgramArguments</key>"),
            plist_string_after(content, "<key>MAILYPOPPINS_DATA_DIR</key>"),
        ),
    }
}

/// The first word of a systemd value: a double-quoted one with `\"` and `\\`
/// read back, which is what [`escape`] writes, or a bare one up to the first
/// space.
fn first_systemd_word(value: &str) -> Option<String> {
    let value = value.trim_start();
    let Some(quoted) = value.strip_prefix('"') else {
        return value.split_whitespace().next().map(str::to_string);
    };
    let mut out = String::new();
    let mut chars = quoted.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next()?),
            '"' => return Some(out),
            c => out.push(c),
        }
    }
    None
}

/// The first `<string>` after `key` in a plist, with the entities read back.
fn plist_string_after(content: &str, key: &str) -> Option<String> {
    let rest = &content[content.find(key)? + key.len()..];
    let start = rest.find("<string>")? + "<string>".len();
    let end = start + rest[start..].find("</string>")?;
    Some(
        rest[start..end]
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

fn print_lines(lines: &[String]) {
    for line in lines {
        println!("{line}");
    }
}

fn tick() -> colored::ColoredString {
    "\u{2713}".green()
}

fn cross() -> colored::ColoredString {
    "\u{2717}".red()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::daemon::shutdown::DEFAULT_GRACE_SECS;

    /// The executable is baked as the OS named it: a symlink is kept rather
    /// than resolved into the directory it points at, and a `PATH` entry that
    /// is the same file wins over the real path.
    #[cfg(unix)]
    #[test]
    fn the_service_exe_keeps_a_symlink_and_prefers_the_same_file_on_path() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tempdir");
        let cellar = tmp.path().join("Cellar").join("1.0").join("bin");
        let bin = tmp.path().join("bin");
        fs::create_dir_all(&cellar).expect("cellar");
        fs::create_dir_all(&bin).expect("bin");
        let real = cellar.join("mp");
        fs::write(&real, b"#!/bin/sh\n").expect("real mp");
        fs::set_permissions(&real, fs::Permissions::from_mode(0o755)).expect("chmod");
        let link = bin.join("mp");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        assert_eq!(service_exe(link.clone(), None), link, "the symlink is kept");
        assert_eq!(
            service_exe(real.clone(), Some(bin.clone().into_os_string())),
            link,
            "the PATH entry that is the same file is baked"
        );
        let other = tmp.path().join("other");
        fs::create_dir_all(&other).expect("other");
        fs::write(other.join("mp"), b"#!/bin/sh\n").expect("other mp");
        fs::set_permissions(other.join("mp"), fs::Permissions::from_mode(0o755)).expect("chmod");
        assert_eq!(
            service_exe(real.clone(), Some(other.into_os_string())),
            real,
            "another file on PATH is not this one"
        );
    }

    /// A relative `XDG_CONFIG_HOME` is invalid by the spec and falls back.
    #[test]
    fn a_relative_xdg_config_home_falls_back_to_home() {
        let home = PathBuf::from("/home/u");
        assert_eq!(
            xdg_config_home_from(Some("rel".into()), home.clone()),
            home.join(".config")
        );
        assert_eq!(
            xdg_config_home_from(Some("".into()), home.clone()),
            home.join(".config")
        );
        assert_eq!(
            xdg_config_home_from(None, home.clone()),
            home.join(".config")
        );
        assert_eq!(
            xdg_config_home_from(Some("/x/cfg".into()), home),
            PathBuf::from("/x/cfg")
        );
    }

    fn fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("service")
            .join(name);
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    /// The compiled-in templates are the committed fixtures, byte for byte.
    ///
    /// The production code does not `include_str!` out of `tests/` - a library
    /// that builds only when its test fixtures are present is a library with a
    /// second source tree - so the copy is checked here instead. A fixture edit
    /// that does not reach `src/daemon/templates/` fails in this file rather
    /// than twenty-three rows later.
    #[test]
    fn the_templates_are_the_committed_fixtures() {
        assert_eq!(UNIT_TEMPLATE, fixture(UNIT_NAME));
        assert_eq!(PLIST_TEMPLATE, fixture(&format!("{LAUNCHD_LABEL}.plist")));
    }

    /// The service manager's stop timeout is the daemon's own grace plus a
    /// margin, in both files.
    ///
    /// The literal lives in the templates, so this is what ties it to the
    /// constant: a grace raised in `src/daemon/shutdown.rs` fails here, where
    /// the fix is one line in each template, rather than in production, where
    /// it is a `SIGKILL` in the middle of the eight shutdown steps.
    #[test]
    fn the_stop_timeout_follows_the_shutdown_grace() {
        let expected = DEFAULT_GRACE_SECS + STOP_MARGIN_SECS;
        assert!(
            UNIT_TEMPLATE.contains(&format!("\nTimeoutStopSec={expected}\n")),
            "the unit's TimeoutStopSec is {expected}:\n{UNIT_TEMPLATE}"
        );
        assert!(
            PLIST_TEMPLATE.contains(&format!(
                "<key>ExitTimeOut</key>\n\t<integer>{expected}</integer>"
            )),
            "and so is the agent's ExitTimeOut:\n{PLIST_TEMPLATE}"
        );
    }

    /// Unset means this build; the two names are the only ones accepted.
    #[test]
    fn the_os_override_takes_two_names_and_nothing_else() {
        let native = if cfg!(target_os = "macos") {
            Target::Darwin
        } else {
            Target::Linux
        };
        assert_eq!(resolve_target(None).unwrap(), native);
        assert_eq!(resolve_target(Some("")).unwrap(), native);
        assert_eq!(resolve_target(Some(" ")).unwrap(), native);
        assert_eq!(resolve_target(Some("linux")).unwrap(), Target::Linux);
        assert_eq!(resolve_target(Some(" darwin ")).unwrap(), Target::Darwin);

        let refusal = resolve_target(Some("plan9")).unwrap_err().to_string();
        for named in [OS_ENV, "plan9", "linux", "darwin"] {
            assert!(
                refusal.contains(named),
                "the refusal names {named}: {refusal}"
            );
        }
    }

    /// Every placeholder is substituted, and none survives into a written file.
    #[test]
    fn rendering_leaves_no_placeholder_behind() {
        for target in [Target::Linux, Target::Darwin] {
            let rendered = render_template(target, "/opt/mp", "/data", "/config");
            assert!(!rendered.contains("{{"), "rendered:\n{rendered}");
            assert!(rendered.contains("/opt/mp"));
            assert!(rendered.contains("/data"));
            assert!(rendered.contains("/config"));
        }
    }

    /// A path with a space and an `&` in it survives into both files as one
    /// value, and neither file can be made to mean something else by it.
    ///
    /// A `$HOME` with a space in it is ordinary on macOS and reachable on
    /// Linux, and `MAILYPOPPINS_DATA_DIR` is whatever the user exported. An
    /// unquoted `ExecStart` would split it into two argv entries, an unquoted
    /// `Environment=` would drop everything after the space, and a raw `&` in
    /// a plist `<string>` is an XML parse error rather than a path.
    #[test]
    fn a_path_with_a_space_and_an_ampersand_renders_as_one_value() {
        let mp = "/home/a b/Mail & More/bin/mp";
        let data = "/home/a b/Mail & More/data";
        let config = "/home/a b/Mail & More/config";

        let unit = render_template(Target::Linux, mp, data, config);
        assert!(
            unit.contains(&format!("ExecStart=\"{mp}\" daemon run\n")),
            "the executable is one double-quoted argument; unit:\n{unit}"
        );
        assert!(
            unit.contains(&format!(
                "Environment=\"MAILYPOPPINS_DATA_DIR={data}\"\nEnvironment=\"MAILYPOPPINS_CONFIG_DIR={config}\"\n"
            )),
            "and each assignment is one double-quoted word, space and all; unit:\n{unit}"
        );

        let plist = render_template(Target::Darwin, mp, data, config);
        let escaped = "/home/a b/Mail &amp; More";
        assert!(
            plist.contains(&format!("<string>{escaped}/bin/mp</string>")),
            "the `&` is an entity, so the document still parses; plist:\n{plist}"
        );
        assert!(
            !plist.replace("&amp;", "").contains('&'),
            "every ampersand in the document is an entity; plist:\n{plist}"
        );
    }

    /// The two characters each format reads specially are escaped, and only
    /// those.
    #[test]
    fn the_escape_is_the_one_each_format_needs() {
        assert_eq!(
            escape(Target::Linux, r#"/a b/c"d\e"#),
            r#"/a b/c\"d\\e"#,
            "systemd reads `\\` and `\"` inside a double-quoted value"
        );
        assert_eq!(
            escape(Target::Darwin, r#"/a b/c&d<e>f"g"#),
            "/a b/c&amp;d&lt;e&gt;f&quot;g",
            "XML reads four, and a backslash is not one of them"
        );
        assert_eq!(escape(Target::Linux, "/plain/path"), "/plain/path");
        assert_eq!(escape(Target::Darwin, "/plain/path"), "/plain/path");
    }

    /// The order of the two `systemctl` lines is the contract, in both
    /// directions.
    #[test]
    fn the_systemd_commands_reload_before_enabling_and_disable_before_reloading() {
        let path = Path::new("/tmp/mailypoppins.service");
        let install: Vec<String> = install_commands(Target::Linux, path)
            .iter()
            .map(Invocation::display)
            .collect();
        assert_eq!(
            install,
            vec![
                "systemctl --user daemon-reload".to_string(),
                format!("systemctl --user enable --now {UNIT_NAME}"),
            ]
        );
        let uninstall: Vec<String> = uninstall_commands(Target::Linux, path)
            .iter()
            .map(Invocation::display)
            .collect();
        assert_eq!(
            uninstall,
            vec![
                format!("systemctl --user disable --now {UNIT_NAME}"),
                "systemctl --user daemon-reload".to_string(),
            ]
        );
    }

    /// A handover registers the unit without starting it, since its start is
    /// the manager's restart once the running daemon is stopped; on macOS
    /// `bootstrap` is the start, so nothing registers ahead of it.
    #[test]
    fn a_handover_registers_the_service_without_starting_it() {
        let register: Vec<String> = register_commands(Target::Linux)
            .iter()
            .map(Invocation::display)
            .collect();
        assert_eq!(
            register,
            vec![
                "systemctl --user daemon-reload".to_string(),
                format!("systemctl --user enable {UNIT_NAME}"),
            ]
        );
        assert!(register_commands(Target::Darwin).is_empty());
    }

    /// `bootstrap` takes the plist, `bootout` takes the label.
    #[test]
    fn the_launchd_commands_bootstrap_a_path_and_boot_out_a_label() {
        let path = Path::new("/home/x/Library/LaunchAgents/dev.mailypoppins.daemon.plist");
        let install = install_commands(Target::Darwin, path);
        assert_eq!(
            install[0].display(),
            format!("launchctl bootstrap {} {}", gui_domain(), path.display())
        );
        let uninstall = uninstall_commands(Target::Darwin, path);
        assert_eq!(
            uninstall[0].display(),
            format!("launchctl bootout {}/{LAUNCHD_LABEL}", gui_domain())
        );
    }

    /// The note under the command block says which of the three things
    /// happened, and says nothing when the commands really ran.
    #[test]
    fn the_note_names_the_hook_or_the_missing_manager() {
        assert_eq!(
            note(Mode::DryRun, "systemctl", "enable").unwrap(),
            "  dry run: MAILYPOPPINS_DAEMON_SERVICE_DRY_RUN is set, nothing was run"
        );
        assert_eq!(
            note(Mode::NoManager, "systemctl", "enable").unwrap(),
            "  systemctl is not on PATH; run the lines above to enable the service"
        );
        assert!(note(Mode::Run, "systemctl", "enable").is_none());
    }

    /// A captured command that fails says in its error what it printed, on
    /// stderr and on stdout, and one that succeeds says nothing anywhere.
    #[test]
    fn a_captured_command_puts_what_it_printed_into_its_error() {
        let failing = Invocation::new(
            "sh",
            &strings(&["-c", "echo 'Job failed.' >&2; echo out; echo; exit 3"]),
        );
        let error = run_one(&failing, ManagerOutput::Captured)
            .unwrap_err()
            .to_string();
        assert!(
            error.ends_with("exited with 3: Job failed.; out"),
            "{error}"
        );

        let silent = Invocation::new("sh", &strings(&["-c", "exit 3"]));
        let error = run_one(&silent, ManagerOutput::Captured)
            .unwrap_err()
            .to_string();
        assert!(error.ends_with("exited with 3"), "{error}");

        let fine = Invocation::new("sh", &strings(&["-c", "echo chatty >&2"]));
        run_one(&fine, ManagerOutput::Captured).expect("a command that succeeds");
    }

    /// `PATH` is what decides whether a manager is reachable, and an empty
    /// directory means it is not.
    #[test]
    fn on_path_finds_an_executable_and_ignores_an_empty_directory() {
        assert!(on_path("sh"), "`sh` is on the test runner's PATH");
        assert!(!on_path("a-program-nobody-installed"));
    }

    fn route_lines(route: &StartRoute) -> Vec<String> {
        match route {
            StartRoute::Supervised(plan) => plan.lines(),
            StartRoute::Detached { .. } => panic!("expected a supervised start, got {route:?}"),
        }
    }

    fn route_note(route: &StartRoute) -> Option<String> {
        match route {
            StartRoute::Detached { note } => note.clone(),
            StartRoute::Supervised(_) => panic!("expected a detached start, got {route:?}"),
        }
    }

    /// No service, or another data directory's, starts a detached daemon and
    /// says nothing, whatever the service manager would have answered.
    #[test]
    fn a_start_without_this_data_directorys_service_is_detached_and_silent() {
        let path = Path::new("/s");
        for target in [Target::Linux, Target::Darwin] {
            for installed in [Installed::Absent, Installed::Foreign] {
                for (on_path, loaded) in [(false, false), (true, false), (true, true)] {
                    for kind in [StartKind::Start, StartKind::Restart] {
                        let route = decide_start(
                            target,
                            path,
                            kind,
                            installed.clone(),
                            on_path,
                            None,
                            loaded,
                        );
                        assert_eq!(route_note(&route), None, "{target:?} {installed:?}");
                    }
                }
            }
        }
    }

    /// This data directory's service running another executable is passed
    /// over, and the note names the service and the executable.
    #[test]
    fn a_service_for_another_executable_is_passed_over_with_a_note() {
        let route = decide_start(
            Target::Linux,
            Path::new("/s"),
            StartKind::Start,
            Installed::OtherExe("/opt/old/mp".into()),
            true,
            None,
            false,
        );
        let note = route_note(&route).expect("a note");
        assert!(
            note.contains(UNIT_NAME) && note.contains("/opt/old/mp"),
            "{note}"
        );
    }

    /// This service with no manager on `PATH` falls back to a detached start
    /// and says which program was missing.
    #[test]
    fn this_service_without_its_manager_falls_back_with_a_note() {
        for (target, program) in [(Target::Linux, "systemctl"), (Target::Darwin, "launchctl")] {
            let route = decide_start(
                target,
                Path::new("/s"),
                StartKind::Start,
                Installed::This,
                false,
                None,
                false,
            );
            let note = route_note(&route).expect("a note");
            assert!(note.contains(program), "{note}");
        }
    }

    /// This unit with `systemctl` on `PATH` but no user manager behind it (WSL
    /// without systemd, a container, cron) falls back to a detached start, and
    /// the note carries the reason the probe gave.
    #[test]
    fn this_unit_without_a_reachable_user_manager_falls_back_with_a_note() {
        let reason = "XDG_RUNTIME_DIR is not set".to_string();
        let route = decide_start(
            Target::Linux,
            Path::new("/s"),
            StartKind::Restart,
            Installed::This,
            true,
            Some(reason.clone()),
            false,
        );
        let note = route_note(&route).expect("a note");
        assert!(
            note.contains(UNIT_NAME)
                && note.contains("systemd user manager")
                && note.contains(&reason),
            "{note}"
        );
    }

    /// The probe answers `None` only for a socket at
    /// `$XDG_RUNTIME_DIR/systemd/private`, and names what it found otherwise.
    #[test]
    fn the_user_manager_probe_wants_the_private_socket() {
        assert_eq!(
            user_manager_unreachable(None).as_deref(),
            Some("XDG_RUNTIME_DIR is not set")
        );
        assert_eq!(
            user_manager_unreachable(Some(OsStr::new(""))).as_deref(),
            Some("XDG_RUNTIME_DIR is not set")
        );

        let tmp = tempfile::tempdir().unwrap();
        let missing = user_manager_unreachable(Some(tmp.path().as_os_str())).expect("missing");
        assert!(
            missing.ends_with("systemd/private does not exist"),
            "{missing}"
        );

        let systemd = tmp.path().join("systemd");
        fs::create_dir_all(&systemd).unwrap();
        fs::write(systemd.join("private"), "").unwrap();
        let plain = user_manager_unreachable(Some(tmp.path().as_os_str())).expect("plain");
        assert!(
            plain.ends_with("systemd/private is not a socket"),
            "{plain}"
        );

        fs::remove_file(systemd.join("private")).unwrap();
        let _listener = std::os::unix::net::UnixListener::bind(systemd.join("private")).unwrap();
        assert_eq!(user_manager_unreachable(Some(tmp.path().as_os_str())), None);
    }

    /// This service with its manager reachable goes through the manager: one
    /// `systemctl --user start`, or `kickstart` / `bootstrap` by whether
    /// launchd has the agent loaded.
    #[test]
    fn this_service_starts_through_its_manager() {
        let start = |target, path, loaded| {
            let route = decide_start(
                target,
                path,
                StartKind::Start,
                Installed::This,
                true,
                None,
                loaded,
            );
            route_lines(&route)
        };
        let unit = Path::new("/home/u/.config/systemd/user/mailypoppins.service");
        assert_eq!(
            start(Target::Linux, unit, false),
            [format!("  systemctl --user start {UNIT_NAME}")]
        );

        let plist = Path::new("/Users/u/Library/LaunchAgents/dev.mailypoppins.daemon.plist");
        assert_eq!(
            start(Target::Darwin, plist, true),
            [format!(
                "  launchctl kickstart {}/{LAUNCHD_LABEL}",
                gui_domain()
            )]
        );
        assert_eq!(
            start(Target::Darwin, plist, false),
            [format!(
                "  launchctl bootstrap {} {}",
                gui_domain(),
                plist.display()
            )]
        );
    }

    /// The start half of a restart is the manager's own restart, which stops
    /// the instance it may still count running after the socket stop and then
    /// starts the service (PERSO-109); an agent launchd has not loaded is
    /// bootstrapped as for a start.
    #[test]
    fn this_service_restarts_through_its_managers_restart() {
        let restart = |target, path, loaded| {
            let route = decide_start(
                target,
                path,
                StartKind::Restart,
                Installed::This,
                true,
                None,
                loaded,
            );
            route_lines(&route)
        };
        let unit = Path::new("/home/u/.config/systemd/user/mailypoppins.service");
        assert_eq!(
            restart(Target::Linux, unit, false),
            [format!("  systemctl --user restart {UNIT_NAME}")]
        );

        let plist = Path::new("/Users/u/Library/LaunchAgents/dev.mailypoppins.daemon.plist");
        assert_eq!(
            restart(Target::Darwin, plist, true),
            [format!(
                "  launchctl kickstart -k {}/{LAUNCHD_LABEL}",
                gui_domain()
            )]
        );
        assert_eq!(
            restart(Target::Darwin, plist, false),
            [format!(
                "  launchctl bootstrap {} {}",
                gui_domain(),
                plist.display()
            )]
        );
    }

    /// What a rendered service file bakes reads back as the values that went
    /// in, a space, an `&` and a quote included.
    #[test]
    fn a_rendered_service_reads_back_its_executable_and_data_directory() {
        let mp = r#"/home/a b/Mail & "More"/bin/mp"#;
        let data = r#"/home/a b/Mail & "More"/data"#;
        for target in [Target::Linux, Target::Darwin] {
            let rendered = render_template(target, mp, data, "/config");
            assert_eq!(
                baked(target, &rendered),
                (Some(mp.to_string()), Some(data.to_string())),
                "{target:?}:\n{rendered}"
            );
        }
        assert_eq!(baked(Target::Linux, "[Service]\n"), (None, None));
        assert_eq!(
            baked(
                Target::Linux,
                "ExecStart=/bin/mp daemon run\nEnvironment=MAILYPOPPINS_DATA_DIR=/d\n"
            ),
            (Some("/bin/mp".into()), Some("/d".into())),
            "an unquoted hand edit reads too"
        );
    }

    /// A service file is this `mp`'s only for this data directory and this
    /// executable, symlinks followed.
    #[cfg(unix)]
    #[test]
    fn classify_matches_the_data_directory_and_the_executable() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        let other_data = tmp.path().join("other-data");
        fs::create_dir_all(&data).expect("data");
        fs::create_dir_all(&other_data).expect("other data");
        let exe = tmp.path().join("mp");
        fs::write(&exe, b"#!/bin/sh\n").expect("mp");
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).expect("chmod");
        let link = tmp.path().join("mp-link");
        std::os::unix::fs::symlink(&exe, &link).expect("symlink");
        let other_exe = tmp.path().join("other-mp");
        fs::write(&other_exe, b"#!/bin/sh\n").expect("other mp");

        for target in [Target::Linux, Target::Darwin] {
            let file = |mp: &Path, dir: &Path| {
                render_template(
                    target,
                    &mp.display().to_string(),
                    &canonical(dir),
                    "/config",
                )
            };
            assert_eq!(
                classify(target, &file(&link, &data), &data, &exe),
                Installed::This,
                "{target:?}: a symlink to this executable is this executable"
            );
            assert_eq!(
                classify(target, &file(&exe, &other_data), &data, &exe),
                Installed::Foreign,
                "{target:?}: another data directory is not ours"
            );
            assert_eq!(
                classify(target, &file(&other_exe, &data), &data, &exe),
                Installed::OtherExe(other_exe.display().to_string()),
                "{target:?}: another executable is named"
            );
            assert_eq!(
                classify(target, "garbage", &data, &exe),
                Installed::Foreign,
                "{target:?}: an unreadable file is not ours"
            );
        }
    }
}
