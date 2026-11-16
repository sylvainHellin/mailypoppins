//! Handing the notes to the coding agent (PERSO-315): the commands that read
//! `[agent]` and start it.
//!
//! A hand-off is the frontend's: it asks `note.export` for the batch prompt,
//! starts the agent on it here, and calls `note.handoff` with the exported
//! ids once the start succeeded, so a note is never marked handed off for an
//! agent that did not start.
//!
//! # The settings
//!
//! The daemon does not serve `[agent]`: `config.get` answers the keys the
//! Settings view shows, and starting the agent is a client's work. So this
//! layer reads it as the TUI does, with `mp_core::config::load_global_config`,
//! from the file the daemon reads: the app and the daemon share
//! `mp_core::config::config_dir`, and the connector refuses a daemon serving
//! another one. It is read again at every hand-off, so an edited `[agent]`
//! needs no restart. Fixture mode reads the built-in defaults.
//!
//! # The start
//!
//! The prompt is written to `<data>/notes/batch-<date>-<millis>.md`, in a
//! directory private to the user ([`agent::batch_path`]), `{batch}` and
//! `{date}` are replaced in every word of the command, and its program is
//! located as the embedded editor's is: on the login shell's `PATH`, then in
//! the probe directories. The panel's session is a terminal session
//! ([`crate::terminal`]) on a PTY, in `cwd` with `~` expanded, with the
//! editor's environment. `paseo` runs `paseo run --background` and answers
//! what it printed. In fixture mode nothing starts: the command is journaled
//! as the editor's is.

use std::path::{Path, PathBuf};

use mp_core::config::AgentConfig;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;

use crate::agent::{self, AgentSettings};
use crate::editor::EditorSource;
use crate::error::GuiError;
use crate::session::SessionHandle;
use crate::terminal::{self, Draft, Launch, Scheme, TerminalStarted, Terminals};

fn setup(message: String) -> GuiError {
    GuiError::Setup { message }
}

fn task_failed(e: impl std::fmt::Display) -> GuiError {
    GuiError::internal(format!("the command task failed: {e}"))
}

/// The settings in force: the built-in ones in fixture mode, else
/// `config.toml`'s.
fn load_settings(fixture: bool) -> Result<AgentSettings, GuiError> {
    let config = if fixture {
        AgentConfig::default()
    } else {
        mp_core::config::load_global_config()
            .map_err(|e| setup(format!("The [agent] settings did not load: {e:#}")))?
            .agent
    };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    agent::settings(&config, home.as_deref())
        .map_err(|why| setup(format!("{why}; fix [agent] in config.toml.")))
}

/// Write `text` to a fresh batch file under `data_dir`, answering its path.
fn write_batch(data_dir: &Path, text: &str, date: &str) -> Result<String, GuiError> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let path = agent::batch_path(data_dir, date, millis);
    if let Some(dir) = path.parent() {
        mp_core::config::create_private_dir_all(dir).map_err(|e| {
            GuiError::internal(format!("could not create {}: {e:#}", dir.display()))
        })?;
    }
    std::fs::write(&path, text)
        .map_err(|e| GuiError::internal(format!("could not write {}: {e}", path.display())))?;
    Ok(path.to_string_lossy().into_owned())
}

/// `argv` with its program located on the login shell's `PATH` or in the
/// probe directories; `fixture` keeps a program found nowhere bare.
fn located(mut argv: Vec<String>, fixture: bool) -> Result<Vec<String>, GuiError> {
    let home = std::env::var("HOME").ok();
    let dirs = terminal::search_dirs(&terminal::login_env().path, home.as_deref());
    match terminal::find(&argv[0], &dirs, &|p: &Path| p.is_file()) {
        Some(at) => argv[0] = at,
        None if fixture => {}
        None => {
            return Err(setup(format!(
                "`{}` from [agent] was not found on the login shell's PATH; \
                 set its full path in [agent.commands] of config.toml.",
                argv[0]
            )))
        }
    }
    Ok(argv)
}

/// The directory the agent starts in, which must exist.
fn checked_cwd(cwd: &str, fixture: bool) -> Result<PathBuf, GuiError> {
    let dir = PathBuf::from(cwd);
    if !fixture && !dir.is_dir() {
        return Err(setup(format!(
            "The agent's directory {cwd} does not exist; create it or set [agent] cwd in config.toml."
        )));
    }
    Ok(dir)
}

/// The data directory the batch goes to: the fixture's own in fixture mode.
fn data_dir(daemon: &SessionHandle) -> PathBuf {
    match daemon.fixture() {
        Some(f) => f.root(),
        None => crate::paths::data_dir(),
    }
}

/// The agent `[agent]` names and where it opens.
#[tauri::command(rename_all = "snake_case")]
pub async fn agent_settings(daemon: State<'_, SessionHandle>) -> Result<AgentSettings, GuiError> {
    let fixture = daemon.fixture().is_some();
    tauri::async_runtime::spawn_blocking(move || load_settings(fixture))
        .await
        .map_err(task_failed)?
}

/// Start the agent on `text`, the batch prompt, in a terminal session of
/// `cols` by `rows` in the app's palette `theme`; `date` is today, as the
/// webview names it. Output and the exit come on `output`, as an editor's.
#[tauri::command(rename_all = "snake_case")]
#[allow(clippy::too_many_arguments)]
pub async fn agent_spawn(
    daemon: State<'_, SessionHandle>,
    terminals: State<'_, Terminals>,
    text: String,
    date: String,
    cols: u16,
    rows: u16,
    theme: String,
    output: Channel<InvokeResponseBody>,
) -> Result<TerminalStarted, GuiError> {
    let scheme = Scheme::parse(&theme)?;
    let fixture = daemon.fixture();
    let data = data_dir(&daemon);
    let terminals = terminals.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let date = agent::checked_date(&date)
            .map_err(GuiError::protocol)?
            .to_string();
        let settings = load_settings(fixture.is_some())?;
        let cwd = checked_cwd(&settings.cwd, fixture.is_some())?;
        let batch = write_batch(&data, &text, &date)?;
        let argv = located(
            agent::substitute(&settings.command, &batch, &date),
            fixture.is_some(),
        )?;
        let login = terminal::login_env();
        let mut env = vec![
            ("PATH".to_string(), login.path.clone()),
            ("TERM".to_string(), "xterm-256color".to_string()),
            ("COLORTERM".to_string(), "truecolor".to_string()),
            (terminal::THEME_ENV.to_string(), scheme.name().to_string()),
        ];
        if !terminal::LOCALE_VARS
            .iter()
            .any(|v| std::env::var_os(v).is_some())
        {
            let lang = login
                .lang
                .clone()
                .unwrap_or_else(|| terminal::DEFAULT_LANG.to_string());
            env.push(("LANG".to_string(), lang));
        }
        let launch = Launch {
            argv,
            cwd,
            env,
            source: EditorSource::Agent,
        };
        // The session table names what a session runs on: here the batch.
        let on = Draft {
            account: String::new(),
            id: settings.name,
            path: batch,
        };
        terminals.start(fixture.as_deref(), on, launch, cols, rows, output)
    })
    .await
    .map_err(task_failed)?
}

/// Start the agent on `text` with a detached `paseo run`, and answer what
/// paseo printed: the agent it created.
#[tauri::command(rename_all = "snake_case")]
pub async fn agent_paseo(
    daemon: State<'_, SessionHandle>,
    text: String,
    date: String,
) -> Result<String, GuiError> {
    let fixture = daemon.fixture();
    let data = data_dir(&daemon);
    tauri::async_runtime::spawn_blocking(move || {
        let date = agent::checked_date(&date)
            .map_err(GuiError::protocol)?
            .to_string();
        let settings = load_settings(fixture.is_some())?;
        checked_cwd(&settings.cwd, fixture.is_some())?;
        let batch = write_batch(&data, &text, &date)?;
        let argv = located(
            agent::paseo_argv(&settings.name, &settings.cwd, &batch, &date),
            fixture.is_some(),
        )?;
        if let Some(fixture) = fixture {
            tracing::info!("[agent] fixture: would run {argv:?}");
            fixture.record_editor(&batch, argv);
            return Ok("fixture".to_string());
        }
        tracing::info!("[agent] {argv:?}");
        let out = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .env("PATH", &terminal::login_env().path)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| setup(format!("paseo did not start: {e}.")))?;
        if !out.status.success() {
            let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(setup(format!("paseo run failed ({}): {why}", out.status)));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    })
    .await
    .map_err(task_failed)?
}
