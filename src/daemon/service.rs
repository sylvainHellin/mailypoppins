//! `mp daemon install-service | uninstall-service` (P6-U6, `LIF-06`).
//!
//! Two local commands that write one file and call one service manager. No
//! daemon has to be running and no wire method is added: asking a running
//! daemon to arrange for a daemon to run would be circular, and the file this
//! writes is the thing that starts one.
//!
//! ## What is written, and where
//!
//! | target | path | template |
//! |---|---|---|
//! | linux | `$XDG_CONFIG_HOME/systemd/user/mailypoppins.service` | [`UNIT_TEMPLATE`] |
//! | darwin | `$HOME/Library/LaunchAgents/dev.mailypoppins.daemon.plist` | [`PLIST_TEMPLATE`] |
//!
//! `$XDG_CONFIG_HOME` falls back to `$HOME/.config`, which is XDG's own rule
//! and systemd's. Both files are written 0644 with their parent directories
//! created as needed, and neither holds a secret.
//!
//! The two templates are byte-for-byte copies of the fixtures
//! `tests/daemon_service.rs` pins, `src/daemon/templates/`, with three
//! placeholders: `{{MP}}` is [`std::env::current_exe`], `{{DATA_DIR}}` and
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

use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
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
pub fn install(force: bool, check: bool) -> Result<i32> {
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
    let mut lines = vec![header];
    lines.extend(commands.iter().map(Invocation::line));
    lines.extend(note(mode, commands[0].program, "enable"));

    let outcome = match mode {
        Mode::Run => run_all(&commands),
        _ => Ok(()),
    };
    print_lines(&lines);
    outcome?;
    Ok(EXIT_OK)
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
            run_all(&commands[..1])?;
        }
        remove_service(&path)?;
        if mode == Mode::Run {
            run_all(&commands[1..])?;
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
    match std::env::var("XDG_CONFIG_HOME") {
        Ok(value) if !value.trim().is_empty() => PathBuf::from(value),
        _ => home_dir().join(".config"),
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
    Ok(render_template(
        template(target),
        &canonical(&exe),
        &canonical(&crate::config::mailypoppins_data_dir()),
        &canonical(&crate::config::config_dir()),
    ))
}

/// The substitution itself, without an environment to read.
fn render_template(template: &str, mp: &str, data_dir: &str, config_dir: &str) -> String {
    template
        .replace("{{MP}}", mp)
        .replace("{{DATA_DIR}}", data_dir)
        .replace("{{CONFIG_DIR}}", config_dir)
}

fn template(target: Target) -> &'static str {
    match target {
        Target::Linux => UNIT_TEMPLATE,
        Target::Darwin => PLIST_TEMPLATE,
    }
}

/// An absolute, symlink-resolved path as a string, which is what the daemon's
/// own runtime metadata carries and therefore what the service file must.
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
        fs::create_dir_all(&logs)
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

fn run_all(commands: &[Invocation]) -> Result<()> {
    for command in commands {
        run_one(command)?;
    }
    Ok(())
}

fn run_one(command: &Invocation) -> Result<()> {
    let status = Command::new(command.program)
        .args(&command.args)
        .status()
        .with_context(|| format!("running `{}`", command.display()))?;
    if !status.success() {
        match status.code() {
            Some(code) => bail!("`{}` exited with {code}", command.display()),
            None => bail!("`{}` was killed by a signal", command.display()),
        }
    }
    Ok(())
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
        for template in [UNIT_TEMPLATE, PLIST_TEMPLATE] {
            let rendered = render_template(template, "/opt/mp", "/data", "/config");
            assert!(!rendered.contains("{{"), "rendered:\n{rendered}");
            assert!(rendered.contains("/opt/mp"));
            assert!(rendered.contains("/data"));
            assert!(rendered.contains("/config"));
        }
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

    /// `PATH` is what decides whether a manager is reachable, and an empty
    /// directory means it is not.
    #[test]
    fn on_path_finds_an_executable_and_ignores_an_empty_directory() {
        assert!(on_path("sh"), "`sh` is on the test runner's PATH");
        assert!(!on_path("a-program-nobody-installed"));
    }
}
