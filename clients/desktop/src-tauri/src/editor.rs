//! The external editor a draft is opened in (M3, #0131).
//!
//! The TUI suspends its terminal and runs `$EDITOR` in it, blocking until the
//! editor exits. A GUI has no terminal to lend, so this spawns the editor
//! detached and never depends on its exit: the daemon's drafts watcher
//! publishes every save as `draft.changed`, and that event is what the
//! frontend follows.
//!
//! # Resolution
//!
//! The editor is a command template, resolved in this order:
//!
//! 1. `MP_DESKTOP_EDITOR`;
//! 2. the `editor` key of `desktop.json` in the app config directory, which
//!    `editor_setting_get` and `editor_setting_set` read and write;
//! 3. `$VISUAL`, then `$EDITOR`; one naming a terminal-only editor
//!    ([`TERMINAL_EDITORS`]) runs inside the first terminal emulator found
//!    ([`terminal_template`]), and is skipped when there is none;
//! 4. the first of [`PROBE_NAMES`] found in [`PROBE_DIRS`];
//! 5. `open -t` on macOS, `xdg-open` elsewhere.
//!
//! `MP_DESKTOP_EDITOR` and the setting are taken verbatim and never wrapped:
//! a terminal editor there names its terminal itself, as in
//! `wezterm start -- nvim {path}`.
//!
//! A template is split with shell-words rules (quotes and backslashes, no
//! expansion, no shell). `{path}` in any word is replaced by the draft's path;
//! a template without it gets the path as its last argument. A bare program
//! name is looked up on `PATH` and then in [`PROBE_DIRS`], because an app
//! started from Finder inherits a `PATH` without Homebrew in it.
//!
//! # Launch
//!
//! The process gets null stdio and its own process group, and the command
//! watches it for [`EXIT_WINDOW`]: a spawn failure or a nonzero exit inside
//! the window is a [`GuiError::Setup`] naming the command, a zero exit (the
//! `code`-style launcher that hands the file to a running app) or a process
//! still running is a success. A process still running after the window is
//! reaped by a thread, so it never lingers as a zombie.
//!
//! In fixture mode nothing is spawned: the resolved command is journaled on
//! the fixture, which `fixture_simulate("editor_save")` then plays against.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Map, Value};
use tauri::{AppHandle, Manager, State};

use crate::error::GuiError;
use crate::fixture::Fixture;
use crate::session::SessionHandle;

/// The environment override of the editor command.
pub const EDITOR_ENV: &str = "MP_DESKTOP_EDITOR";

/// The desktop settings file, in the app config directory.
pub const SETTINGS_FILE: &str = "desktop.json";

/// The key of [`SETTINGS_FILE`] the editor template lives under.
const SETTINGS_KEY: &str = "editor";

/// GUI editors probed for when nothing names one, in this order.
pub const PROBE_NAMES: &[&str] = &["code", "zed", "subl", "cursor"];

/// Where the probes look, and where a bare program name is looked up after
/// `PATH`.
pub const PROBE_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"];

/// How long a launch is watched for an early failure.
pub const EXIT_WINDOW: Duration = Duration::from_secs(2);

/// Editors that need a terminal; `$VISUAL` or `$EDITOR` naming one runs in a
/// terminal emulator, or is skipped when none is found. An explicit
/// `MP_DESKTOP_EDITOR` or setting is never wrapped nor skipped.
pub const TERMINAL_EDITORS: &[&str] = &[
    "vi", "vim", "nvim", "hx", "helix", "nano", "pico", "micro", "kak", "joe", "ne", "mg", "ed",
];

/// A terminal emulator a terminal editor can run in.
pub struct TerminalApp {
    /// The program, looked up on `PATH` and in [`PROBE_DIRS`].
    pub program: &'static str,
    /// The macOS app bundle's name in `/Applications`, without `.app`.
    pub bundle: &'static str,
    /// The words between the terminal and the editor's own command line.
    pub args: &'static [&'static str],
    /// Whether macOS starts it with `open -na <bundle> --args`, since its
    /// binary refuses to start a terminal from the command line there.
    pub macos_open: bool,
}

/// The terminal emulators probed for a terminal editor, in this order.
/// macOS then falls back to Terminal.app ([`TERMINAL_APP`]), anything else to
/// `x-terminal-emulator -e`.
pub const TERMINALS: &[TerminalApp] = &[
    TerminalApp {
        program: "wezterm",
        bundle: "WezTerm",
        args: &["start", "--"],
        macos_open: false,
    },
    TerminalApp {
        program: "ghostty",
        bundle: "Ghostty",
        args: &["-e"],
        macos_open: true,
    },
    TerminalApp {
        program: "kitty",
        bundle: "kitty",
        args: &["--"],
        macos_open: false,
    },
    TerminalApp {
        program: "alacritty",
        bundle: "Alacritty",
        args: &["-e"],
        macos_open: false,
    },
];

/// Terminal.app's binary, whose presence makes it the macOS last resort.
pub const TERMINAL_APP: &str =
    "/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal";

/// The AppleScript that runs its arguments, each shell-quoted, in a new
/// Terminal.app window: the editor's words and the path arrive as `argv`, so
/// neither is ever spliced into the script's text.
const TERMINAL_APP_SCRIPT: &[&str] = &[
    "on run argv",
    "set c to \"\"",
    "repeat with w in argv",
    "set c to c & quoted form of (w as text) & \" \"",
    "end repeat",
    "tell application \"Terminal\" to activate",
    "tell application \"Terminal\" to do script c",
    "end run",
];

/// The Linux last resort, Debian's alternatives name for the default terminal.
const LINUX_TERMINAL: &str = "x-terminal-emulator";

/// Where the editor command came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(rename_all = "snake_case")]
pub enum EditorSource {
    /// `MP_DESKTOP_EDITOR`.
    Env,
    /// The `editor` key of `desktop.json`.
    Setting,
    /// `$VISUAL`.
    Visual,
    /// `$EDITOR`.
    Editor,
    /// A terminal editor from `$VISUAL` or `$EDITOR`, run inside the first
    /// terminal emulator found.
    Terminal,
    /// A GUI editor found in a probe directory.
    Probe,
    /// `open -t` (macOS) or `xdg-open`.
    Fallback,
}

/// A started editor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct EditorLaunch {
    /// The command as it ran (or, in fixture mode, would have run), quoted.
    pub editor: String,
    /// The launched process; absent in fixture mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub source: EditorSource,
    /// Fixture mode: the command was journaled and nothing was launched.
    pub fixture: bool,
}

/// The editor setting and what it resolves to now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct EditorSetting {
    /// The `editor` key of the settings file, `null` when unset.
    pub editor: Option<String>,
    /// The settings file.
    pub file: String,
    /// `MP_DESKTOP_EDITOR`, which wins over the setting when set.
    pub env_override: Option<String>,
    /// The template an `editor_open` would run now.
    pub effective: String,
    pub effective_source: EditorSource,
}

/// A resolved editor template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub template: String,
    pub source: EditorSource,
}

/// What resolution reads, injectable for the tests.
pub struct Lookup<'a> {
    /// An environment variable.
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// The settings file's `editor` key.
    pub setting: Option<String>,
    /// Whether a file is there to run.
    pub is_file: &'a dyn Fn(&Path) -> bool,
    /// macOS, which has `open -t`.
    pub macos: bool,
}

impl Lookup<'_> {
    fn var(&self, name: &str) -> Option<String> {
        (self.env)(name).filter(|v| !v.trim().is_empty())
    }
}

/// The editor template, by the order in the module docs.
pub fn resolve(lookup: &Lookup) -> Resolved {
    let found = |template: String, source| Resolved { template, source };
    if let Some(v) = lookup.var(EDITOR_ENV) {
        return found(v, EditorSource::Env);
    }
    if let Some(v) = lookup.setting.clone().filter(|v| !v.trim().is_empty()) {
        return found(v, EditorSource::Setting);
    }
    for (name, source) in [
        ("VISUAL", EditorSource::Visual),
        ("EDITOR", EditorSource::Editor),
    ] {
        if let Some(v) = lookup.var(name) {
            if needs_terminal(&v) {
                if let Some(template) = terminal_template(lookup, &v) {
                    tracing::info!("[editor] ${name}={v} runs in a terminal: {template}");
                    return found(template, EditorSource::Terminal);
                }
                tracing::info!("[editor] ${name}={v} needs a terminal and none was found; skipped");
                continue;
            }
            return found(v, source);
        }
    }
    for name in PROBE_NAMES {
        for dir in PROBE_DIRS {
            let candidate = Path::new(dir).join(name);
            if (lookup.is_file)(&candidate) {
                return found(quote(&candidate.to_string_lossy()), EditorSource::Probe);
            }
        }
    }
    let fallback = if lookup.macos { "open -t" } else { "xdg-open" };
    found(fallback.to_string(), EditorSource::Fallback)
}

/// `editor` (a `$VISUAL` or `$EDITOR` value, with its own arguments) run
/// inside the first terminal emulator found, as a template that still takes
/// `{path}`; `None` when no terminal is found or `editor` does not split.
///
/// Each of [`TERMINALS`] is looked for on `PATH`, in [`PROBE_DIRS`] and, on
/// macOS, as `/Applications/<bundle>.app/Contents/MacOS/<program>`; macOS then
/// falls back to Terminal.app through `osascript`, anything else to
/// `x-terminal-emulator -e`. The editor's program is located the same way,
/// since the terminal may not see the shell's `PATH`.
pub fn terminal_template(lookup: &Lookup, editor: &str) -> Option<String> {
    let mut words = split(editor).ok()?;
    let program = words.first_mut()?;
    let path_var = lookup.var("PATH");
    *program = locate(program, path_var.as_deref(), lookup.is_file);
    if !words.iter().any(|w| w.contains("{path}")) {
        words.push("{path}".to_string());
    }
    let found_on_path = |program: &str| {
        let at = locate(program, path_var.as_deref(), lookup.is_file);
        (at != program).then_some(at)
    };
    let mut prefix: Option<Vec<String>> = None;
    for t in TERMINALS {
        let bundle = format!("/Applications/{}.app", t.bundle);
        let in_bundle = format!("{bundle}/Contents/MacOS/{}", t.program);
        if lookup.macos && t.macos_open {
            if (lookup.is_file)(Path::new(&in_bundle)) {
                prefix = Some(
                    ["open", "-na", &bundle, "--args"]
                        .into_iter()
                        .chain(t.args.iter().copied())
                        .map(str::to_string)
                        .collect(),
                );
                break;
            }
            continue;
        }
        let at = found_on_path(t.program).or_else(|| {
            (lookup.macos && (lookup.is_file)(Path::new(&in_bundle))).then_some(in_bundle)
        });
        if let Some(at) = at {
            prefix = Some(
                std::iter::once(at)
                    .chain(t.args.iter().map(|a| a.to_string()))
                    .collect(),
            );
            break;
        }
    }
    if prefix.is_none() && lookup.macos && (lookup.is_file)(Path::new(TERMINAL_APP)) {
        let mut words = vec!["osascript".to_string()];
        for line in TERMINAL_APP_SCRIPT {
            words.push("-e".to_string());
            words.push(line.to_string());
        }
        prefix = Some(words);
    }
    if prefix.is_none() && !lookup.macos {
        prefix = found_on_path(LINUX_TERMINAL).map(|at| vec![at, "-e".to_string()]);
    }
    let all = prefix?.into_iter().chain(words);
    let quoted = all.map(|w| if w == "{path}" { w } else { quote(&w) });
    Some(quoted.collect::<Vec<_>>().join(" "))
}

/// Whether a template's program is a terminal-only editor.
fn needs_terminal(template: &str) -> bool {
    split(template)
        .ok()
        .and_then(|w| w.into_iter().next())
        .is_some_and(|program| {
            let name = Path::new(&program)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or(program);
            TERMINAL_EDITORS.contains(&name.as_str())
        })
}

/// Split a command template the way a POSIX shell splits words, without any
/// expansion: single quotes are literal, double quotes honour `\"`, `\\`,
/// `\$` and `` \` ``, and a backslash outside quotes escapes the next
/// character.
pub fn split(template: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = template.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(x) => word.push(x),
                        None => return Err(format!("unclosed single quote in `{template}`")),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(x @ ('"' | '\\' | '$' | '`')) => word.push(x),
                            Some('\n') => {}
                            Some(x) => {
                                word.push('\\');
                                word.push(x);
                            }
                            None => return Err(format!("unclosed double quote in `{template}`")),
                        },
                        Some(x) => word.push(x),
                        None => return Err(format!("unclosed double quote in `{template}`")),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some('\n') => {}
                    Some(x) => word.push(x),
                    None => return Err(format!("a trailing backslash in `{template}`")),
                }
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

/// One word quoted so [`split`] reads it back unchanged.
pub fn quote(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+=:,@%".contains(c));
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// The argument vector a template runs with on `path`.
pub fn command_line(template: &str, path: &str) -> Result<Vec<String>, String> {
    let mut words = split(template)?;
    if words.is_empty() {
        return Err(format!("the editor command `{template}` is empty"));
    }
    if words.iter().any(|w| w.contains("{path}")) {
        for w in &mut words {
            *w = w.replace("{path}", path);
        }
    } else {
        words.push(path.to_string());
    }
    Ok(words)
}

/// A bare program name found on `path_var`, then in [`PROBE_DIRS`]; a name
/// with a slash, or one found nowhere, is left as it is.
pub fn locate(program: &str, path_var: Option<&str>, is_file: &dyn Fn(&Path) -> bool) -> String {
    if program.contains('/') {
        return program.to_string();
    }
    let dirs = path_var
        .into_iter()
        .flat_map(|p| p.split(':'))
        .filter(|d| !d.is_empty())
        .chain(PROBE_DIRS.iter().copied());
    for dir in dirs {
        let candidate = Path::new(dir).join(program);
        if is_file(&candidate) {
            return candidate.to_string_lossy().into_owned();
        }
    }
    program.to_string()
}

/// Spawn `argv` detached and watch it for `window`: the pid, or why it failed.
pub fn spawn_watched(argv: &[String], window: Duration) -> Result<u32, String> {
    let shown = argv.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ");
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| "the editor command is empty".to_string())?;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group, so a Ctrl-C in the terminal that started
        // the app does not take the editor with it.
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("could not start `{shown}`: {e}"))?;
    let pid = child.id();
    let deadline = Instant::now() + window;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(pid),
            Ok(Some(status)) => return Err(format!("`{shown}` exited with {status}")),
            Ok(None) if Instant::now() >= deadline => break,
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => return Err(format!("could not watch `{shown}`: {e}")),
        }
    }
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}

/// The setup error of an editor that would not start.
fn setup_error(why: &str, source: EditorSource) -> GuiError {
    let hint = match source {
        EditorSource::Env => format!("check {EDITOR_ENV}"),
        EditorSource::Setting => "check the editor setting".to_string(),
        EditorSource::Terminal => format!(
            "the terminal comes from $VISUAL or $EDITOR naming a terminal editor; \
             set {EDITOR_ENV} or the editor setting to another, e.g. \"wezterm start -- nvim {{path}}\""
        ),
        _ => format!(
            "set {EDITOR_ENV} or the editor setting, e.g. \"code --wait {{path}}\"; \
             a terminal editor needs a terminal command such as \"wezterm start -- hx {{path}}\""
        ),
    };
    GuiError::Setup {
        message: format!("The editor did not start: {why}; {hint}."),
    }
}

/// Open `path` in the resolved editor, or, with a fixture, journal the command
/// and open nothing.
pub fn open_on(
    fixture: Option<&Fixture>,
    lookup: &Lookup,
    path: &str,
    window: Duration,
) -> Result<EditorLaunch, GuiError> {
    let file = Path::new(path);
    if !file.is_absolute() {
        return Err(GuiError::protocol(format!(
            "`{path}` is not an absolute path"
        )));
    }
    if !file.is_file() {
        return Err(GuiError::not_found(format!("no file at {path}")));
    }
    let resolved = resolve(lookup);
    let mut argv =
        command_line(&resolved.template, path).map_err(|why| setup_error(&why, resolved.source))?;
    argv[0] = locate(&argv[0], lookup.var("PATH").as_deref(), lookup.is_file);
    let editor = argv.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ");
    if let Some(fixture) = fixture {
        tracing::info!("[editor] fixture: would run {editor}");
        fixture.record_editor(path, argv);
        return Ok(EditorLaunch {
            editor,
            pid: None,
            source: resolved.source,
            fixture: true,
        });
    }
    tracing::info!("[editor] {editor}");
    let pid = spawn_watched(&argv, window).map_err(|why| setup_error(&why, resolved.source))?;
    Ok(EditorLaunch {
        editor,
        pid: Some(pid),
        source: resolved.source,
        fixture: false,
    })
}

// ---------------------------------------------------------------------------
// The settings file
// ---------------------------------------------------------------------------

/// `desktop.json` in `config_dir`.
pub fn settings_path(config_dir: &Path) -> PathBuf {
    config_dir.join(SETTINGS_FILE)
}

fn read_settings(file: &Path) -> Result<Map<String, Value>, GuiError> {
    match std::fs::read_to_string(file) {
        Ok(text) if text.trim().is_empty() => Ok(Map::new()),
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => Ok(map),
            Ok(_) | Err(_) => Err(GuiError::Setup {
                message: format!("{} is not a JSON object", file.display()),
            }),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(e) => Err(GuiError::internal(format!(
            "could not read {}: {e}",
            file.display()
        ))),
    }
}

/// The `editor` key of the settings file, `None` when the file or the key is
/// missing.
pub fn read_setting(file: &Path) -> Result<Option<String>, GuiError> {
    Ok(read_settings(file)?
        .get(SETTINGS_KEY)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|v| !v.trim().is_empty()))
}

/// Set (or, with `None` or a blank value, clear) the `editor` key, keeping
/// every other key of the file.
pub fn write_setting(file: &Path, editor: Option<&str>) -> Result<(), GuiError> {
    let mut settings = read_settings(file)?;
    match editor.map(str::trim).filter(|v| !v.is_empty()) {
        Some(editor) => {
            split(editor).map_err(|message| GuiError::Setup { message })?;
            settings.insert(SETTINGS_KEY.to_string(), json!(editor));
        }
        None => {
            settings.remove(SETTINGS_KEY);
        }
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| GuiError::internal(format!("could not create {}: {e}", dir.display())))?;
    }
    let text =
        serde_json::to_string_pretty(&Value::Object(settings)).map_err(GuiError::internal)?;
    std::fs::write(file, text + "\n")
        .map_err(|e| GuiError::internal(format!("could not write {}: {e}", file.display())))
}

/// The setting, and what an `editor_open` would run with it now.
pub fn setting_on(file: &Path, lookup: &Lookup) -> EditorSetting {
    let resolved = resolve(lookup);
    EditorSetting {
        editor: lookup.setting.clone(),
        file: file.display().to_string(),
        env_override: lookup.var(EDITOR_ENV),
        effective: resolved.template,
        effective_source: resolved.source,
    }
}

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn is_file(path: &Path) -> bool {
    path.is_file()
}

pub(crate) fn settings_file(app: &AppHandle) -> Result<PathBuf, GuiError> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| GuiError::internal(format!("no app config directory: {e}")))?;
    Ok(settings_path(&dir))
}

/// The process's own lookup, over the settings file's current value.
pub(crate) fn live_lookup(setting: Option<String>) -> Lookup<'static> {
    Lookup {
        env: &process_env,
        setting,
        is_file: &is_file,
        macos: cfg!(target_os = "macos"),
    }
}

/// Open a draft file in the external editor.
#[tauri::command(rename_all = "snake_case")]
pub async fn editor_open(
    app: AppHandle,
    session: State<'_, SessionHandle>,
    path: String,
) -> Result<EditorLaunch, GuiError> {
    let file = settings_file(&app)?;
    let fixture = session.fixture();
    tauri::async_runtime::spawn_blocking(move || {
        let setting = match read_setting(&file) {
            Ok(setting) => setting,
            Err(e) => {
                tracing::warn!("[editor] ignoring the setting: {e}");
                None
            }
        };
        open_on(
            fixture.as_deref(),
            &live_lookup(setting),
            &path,
            EXIT_WINDOW,
        )
    })
    .await
    .map_err(|e| GuiError::internal(format!("the command task failed: {e}")))?
}

#[tauri::command(rename_all = "snake_case")]
pub fn editor_setting_get(app: AppHandle) -> Result<EditorSetting, GuiError> {
    let file = settings_file(&app)?;
    let setting = read_setting(&file)?;
    Ok(setting_on(&file, &live_lookup(setting)))
}

/// Set the editor command template, or clear it with `null`.
#[tauri::command(rename_all = "snake_case")]
pub fn editor_setting_set(
    app: AppHandle,
    editor: Option<String>,
) -> Result<EditorSetting, GuiError> {
    let file = settings_file(&app)?;
    write_setting(&file, editor.as_deref())?;
    let setting = read_setting(&file)?;
    Ok(setting_on(&file, &live_lookup(setting)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn lookup<'a>(
        env: &'a dyn Fn(&str) -> Option<String>,
        setting: Option<&str>,
        is_file: &'a dyn Fn(&Path) -> bool,
    ) -> Lookup<'a> {
        Lookup {
            env,
            setting: setting.map(str::to_string),
            is_file,
            macos: true,
        }
    }

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn the_env_and_the_setting_win_over_visual_editor_and_the_probes() {
        let code = |p: &Path| p == Path::new("/usr/local/bin/code");
        let all = env_of(&[
            (EDITOR_ENV, "zed {path}"),
            ("VISUAL", "subl -w"),
            ("EDITOR", "code -w"),
        ]);
        let r = resolve(&lookup(&all, Some("cursor"), &code));
        assert_eq!(
            (r.template.as_str(), r.source),
            ("zed {path}", EditorSource::Env)
        );
        let no_env = env_of(&[("VISUAL", "subl -w"), ("EDITOR", "code -w")]);
        let r = resolve(&lookup(&no_env, Some("cursor"), &code));
        assert_eq!(
            (r.template.as_str(), r.source),
            ("cursor", EditorSource::Setting)
        );
        let r = resolve(&lookup(&no_env, None, &code));
        assert_eq!(
            (r.template.as_str(), r.source),
            ("subl -w", EditorSource::Visual)
        );
        let editor_only = env_of(&[("EDITOR", "code -w"), ("VISUAL", "  ")]);
        let r = resolve(&lookup(&editor_only, Some(" "), &code));
        assert_eq!(
            (r.template.as_str(), r.source),
            ("code -w", EditorSource::Editor)
        );
    }

    #[test]
    fn a_terminal_editor_in_the_environment_falls_through_to_the_probes() {
        // The earlier name sits in the later directory, so the assertion only
        // holds when the probes go name by name, then directory by directory.
        let files =
            |p: &Path| p == Path::new("/usr/bin/zed") || p == Path::new("/opt/homebrew/bin/cursor");
        let env = env_of(&[("VISUAL", "/usr/bin/nvim"), ("EDITOR", "hx")]);
        let r = resolve(&lookup(&env, None, &files));
        assert_eq!(
            (r.template.as_str(), r.source),
            ("/usr/bin/zed", EditorSource::Probe),
            "zed comes before cursor in the probe order, whatever the directory"
        );
        // An explicit choice is taken as it is, terminal or not.
        let explicit = env_of(&[(EDITOR_ENV, "hx")]);
        assert_eq!(
            resolve(&lookup(&explicit, None, &files)).source,
            EditorSource::Env
        );
    }

    fn files_at(paths: &'static [&'static str]) -> impl Fn(&Path) -> bool {
        move |p| paths.iter().any(|f| p == Path::new(f))
    }

    #[test]
    fn a_terminal_editor_in_the_environment_runs_in_the_first_terminal_found() {
        let files = files_at(&[
            "/opt/homebrew/bin/nvim",
            "/opt/homebrew/bin/wezterm",
            "/opt/homebrew/bin/kitty",
            "/usr/local/bin/code",
            TERMINAL_APP,
        ]);
        let env = env_of(&[("EDITOR", "nvim")]);
        let r = resolve(&lookup(&env, None, &files));
        assert_eq!(
            (r.template.as_str(), r.source),
            (
                "/opt/homebrew/bin/wezterm start -- /opt/homebrew/bin/nvim {path}",
                EditorSource::Terminal
            ),
            "a terminal wins over the GUI probes, and wezterm over kitty"
        );
        assert_eq!(
            command_line(&r.template, "/d/a b.md").expect("line"),
            [
                "/opt/homebrew/bin/wezterm",
                "start",
                "--",
                "/opt/homebrew/bin/nvim",
                "/d/a b.md"
            ]
        );
    }

    #[test]
    fn each_terminal_has_its_own_command_shape_in_probe_order() {
        let env = env_of(&[("EDITOR", "hx"), ("PATH", "/x")]);
        let template = |files: &'static [&'static str]| {
            let is = files_at(files);
            resolve(&lookup(&env, None, &is)).template
        };
        assert_eq!(
            template(&["/Applications/WezTerm.app/Contents/MacOS/wezterm"]),
            "/Applications/WezTerm.app/Contents/MacOS/wezterm start -- hx {path}",
            "an app bundle counts when nothing is on PATH"
        );
        assert_eq!(
            template(&[
                "/Applications/Ghostty.app/Contents/MacOS/ghostty",
                "/x/kitty"
            ]),
            "open -na /Applications/Ghostty.app --args -e hx {path}",
            "Ghostty starts through open on macOS, and before kitty"
        );
        assert_eq!(
            template(&["/x/kitty", "/opt/homebrew/bin/alacritty"]),
            "/x/kitty -- hx {path}"
        );
        assert_eq!(
            template(&["/Applications/Alacritty.app/Contents/MacOS/alacritty"]),
            "/Applications/Alacritty.app/Contents/MacOS/alacritty -e hx {path}"
        );
    }

    #[test]
    fn terminal_app_is_the_macos_last_resort_through_osascript() {
        let files = files_at(&[TERMINAL_APP, "/usr/bin/zed"]);
        let env = env_of(&[("VISUAL", "nvim -u 'my init.lua'")]);
        let r = resolve(&lookup(&env, None, &files));
        assert_eq!(r.source, EditorSource::Terminal);
        let argv = command_line(&r.template, "/d/it's.md").expect("line");
        assert_eq!(argv[0], "osascript");
        assert_eq!(
            &argv[1..17],
            TERMINAL_APP_SCRIPT
                .iter()
                .flat_map(|l| ["-e", l])
                .collect::<Vec<_>>()
                .as_slice()
        );
        assert_eq!(
            &argv[17..],
            ["nvim", "-u", "my init.lua", "/d/it's.md"],
            "the editor's words and the path reach the script as argv, unspliced"
        );
    }

    #[test]
    fn with_no_terminal_a_terminal_editor_falls_through_as_before() {
        let none = |_: &Path| false;
        let env = env_of(&[("VISUAL", "vim"), ("EDITOR", "nvim")]);
        let r = resolve(&lookup(&env, None, &none));
        assert_eq!(
            (r.template.as_str(), r.source),
            ("open -t", EditorSource::Fallback)
        );
        let mut linux = lookup(&env, None, &none);
        linux.macos = false;
        assert_eq!(resolve(&linux).source, EditorSource::Fallback);
        // Terminal.app's binary means nothing off macOS, nor do the bundles.
        let mac_only = files_at(&[
            TERMINAL_APP,
            "/Applications/WezTerm.app/Contents/MacOS/wezterm",
        ]);
        let mut linux = lookup(&env, None, &mac_only);
        linux.macos = false;
        assert_eq!(resolve(&linux).template, "xdg-open");
    }

    #[test]
    fn linux_runs_ghostty_directly_and_falls_back_to_x_terminal_emulator() {
        let env = env_of(&[("EDITOR", "nano")]);
        let ghostty = files_at(&["/usr/bin/ghostty", "/usr/bin/x-terminal-emulator"]);
        let mut l = lookup(&env, None, &ghostty);
        l.macos = false;
        assert_eq!(resolve(&l).template, "/usr/bin/ghostty -e nano {path}");
        let x = files_at(&["/usr/bin/x-terminal-emulator"]);
        let mut l = lookup(&env, None, &x);
        l.macos = false;
        let r = resolve(&l);
        assert_eq!(
            (r.template.as_str(), r.source),
            (
                "/usr/bin/x-terminal-emulator -e nano {path}",
                EditorSource::Terminal
            )
        );
    }

    #[test]
    fn visual_keeps_its_arguments_and_its_own_placeholder() {
        let files = files_at(&["/opt/homebrew/bin/wezterm"]);
        let env = env_of(&[
            ("VISUAL", "nvim -u 'my init.lua' +10 {path}"),
            ("EDITOR", "code -w"),
        ]);
        let r = resolve(&lookup(&env, None, &files));
        assert_eq!(
            (r.template.as_str(), r.source),
            (
                "/opt/homebrew/bin/wezterm start -- nvim -u 'my init.lua' +10 {path}",
                EditorSource::Terminal
            )
        );
        assert_eq!(
            command_line(&r.template, "/d/x.md").expect("line")[3..],
            ["nvim", "-u", "my init.lua", "+10", "/d/x.md"]
        );
    }

    #[test]
    fn a_gui_editor_and_an_explicit_choice_are_never_wrapped() {
        let files = files_at(&["/opt/homebrew/bin/wezterm", TERMINAL_APP]);
        let env = env_of(&[("EDITOR", "zed -w")]);
        let r = resolve(&lookup(&env, None, &files));
        assert_eq!(
            (r.template.as_str(), r.source),
            ("zed -w", EditorSource::Editor)
        );
        let env = env_of(&[(EDITOR_ENV, "nvim"), ("EDITOR", "nvim")]);
        let r = resolve(&lookup(&env, Some("hx"), &files));
        assert_eq!((r.template.as_str(), r.source), ("nvim", EditorSource::Env));
        let env = env_of(&[("EDITOR", "nvim")]);
        let r = resolve(&lookup(&env, Some("hx {path}"), &files));
        assert_eq!(
            (r.template.as_str(), r.source),
            ("hx {path}", EditorSource::Setting)
        );
    }

    #[test]
    fn a_terminal_editor_is_known_by_its_program_name() {
        assert!(needs_terminal("nvim"));
        assert!(needs_terminal("/usr/bin/vim -u x"));
        assert!(needs_terminal("'/opt/my bin/hx' {path}"));
        assert!(!needs_terminal("code -w"));
        assert!(!needs_terminal("nvim-qt"));
        assert!(!needs_terminal("'nvim"), "a template that does not split");
        assert!(!needs_terminal("  "));
    }

    #[test]
    fn with_nothing_found_the_fallback_opens_the_default_app() {
        let none = |_: &Path| false;
        let env = env_of(&[]);
        let r = resolve(&lookup(&env, None, &none));
        assert_eq!(
            (r.template.as_str(), r.source),
            ("open -t", EditorSource::Fallback)
        );
        let mut linux = lookup(&env, None, &none);
        linux.macos = false;
        assert_eq!(resolve(&linux).template, "xdg-open");
        assert_eq!(
            command_line("open -t", "/d/a b.md").expect("line"),
            ["open", "-t", "/d/a b.md"]
        );
    }

    #[test]
    fn the_path_replaces_the_placeholder_or_is_appended() {
        assert_eq!(
            command_line("wezterm start -- hx {path}", "/d/x.md").expect("line"),
            ["wezterm", "start", "--", "hx", "/d/x.md"]
        );
        assert_eq!(
            command_line("code --goto={path}:1", "/d/x.md").expect("line"),
            ["code", "--goto=/d/x.md:1"]
        );
        assert_eq!(
            command_line("'/Applications/My Editor/bin/ed' -w", "/d/x.md").expect("line"),
            ["/Applications/My Editor/bin/ed", "-w", "/d/x.md"]
        );
        assert!(command_line("  ", "/d/x.md").is_err());
    }

    #[test]
    fn a_template_splits_with_shell_words_rules() {
        assert_eq!(
            split(r#"a "b c" 'd "e"' f\ g "h\"i" 'j'k"#).expect("split"),
            ["a", "b c", "d \"e\"", "f g", "h\"i", "jk"]
        );
        assert_eq!(split(r#""""#).expect("split"), [""]);
        assert_eq!(
            split("  $HOME  ").expect("split"),
            ["$HOME"],
            "no expansion"
        );
        assert!(split("'open").is_err());
        assert!(split("\"open").is_err());
        assert!(split("trailing\\").is_err());
        for word in ["plain", "/a b/c", "it's", "", "{path}"] {
            assert_eq!(split(&quote(word)).expect("round trip"), [word]);
        }
    }

    #[test]
    fn a_bare_program_is_found_on_the_path_then_in_the_probe_dirs() {
        let files = |p: &Path| p == Path::new("/opt/homebrew/bin/zed") || p == Path::new("/x/zed");
        assert_eq!(locate("zed", Some("/x:/y"), &files), "/x/zed");
        assert_eq!(
            locate("zed", Some("/usr/bin:/bin"), &files),
            "/opt/homebrew/bin/zed"
        );
        assert_eq!(locate("zed", None, &files), "/opt/homebrew/bin/zed");
        assert_eq!(locate("nowhere", None, &files), "nowhere");
        assert_eq!(locate("./rel/zed", None, &files), "./rel/zed");
    }

    #[test]
    fn a_nonzero_exit_inside_the_window_is_a_setup_error() {
        let window = Duration::from_millis(1500);
        let err = spawn_watched(&["false".to_string()], window).expect_err("false fails");
        assert!(err.contains("exited with"), "{err}");
        assert!(spawn_watched(&["true".to_string()], window).is_ok());
        let err = spawn_watched(&["/no/such/editor".to_string()], window).expect_err("missing");
        assert!(err.contains("could not start"), "{err}");
        // Still running at the end of the window: a success, and the command
        // answers without waiting for it.
        let started = Instant::now();
        let pid = spawn_watched(
            &["sleep".to_string(), "3".to_string()],
            Duration::from_millis(200),
        )
        .expect("running");
        assert!(pid > 0);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn open_on_turns_a_failed_launch_into_a_setup_error_naming_the_setting() {
        let dir = crate::test_support::scratch_dir("editor-open");
        let draft = dir.join("d.md");
        std::fs::write(&draft, "---\nstatus: draft\n---\n").expect("draft");
        let env = env_of(&[("PATH", "/usr/bin:/bin")]);
        let real = |p: &Path| p.is_file();
        let l = lookup(&env, Some("false"), &real);
        let err = open_on(
            None,
            &l,
            &draft.to_string_lossy(),
            Duration::from_millis(1500),
        )
        .expect_err("false exits 1");
        match err {
            GuiError::Setup { message } => {
                assert!(message.contains("editor setting"), "{message}")
            }
            other => panic!("expected a setup error, got {other:?}"),
        }
        assert!(matches!(
            open_on(None, &l, "relative.md", EXIT_WINDOW),
            Err(GuiError::Protocol { .. })
        ));
        assert!(matches!(
            open_on(
                None,
                &l,
                &dir.join("gone.md").to_string_lossy(),
                EXIT_WINDOW
            ),
            Err(GuiError::NotFound { .. })
        ));
    }

    #[test]
    fn the_setting_file_keeps_its_other_keys() {
        let dir = crate::test_support::scratch_dir("editor-setting");
        let file = settings_path(&dir.join("nested"));
        assert_eq!(read_setting(&file).expect("missing is none"), None);
        write_setting(&file, Some("zed -w {path}")).expect("set");
        assert_eq!(
            read_setting(&file).expect("read").as_deref(),
            Some("zed -w {path}")
        );
        let mut other: Value =
            serde_json::from_str(&std::fs::read_to_string(&file).expect("file")).expect("json");
        other["theme"] = json!("dark");
        std::fs::write(&file, other.to_string()).expect("write");
        write_setting(&file, None).expect("clear");
        let after: Value =
            serde_json::from_str(&std::fs::read_to_string(&file).expect("file")).expect("json");
        assert_eq!(after, json!({"theme": "dark"}));
        assert!(matches!(
            write_setting(&file, Some("'unclosed")),
            Err(GuiError::Setup { .. })
        ));
        std::fs::write(&file, "[1]").expect("write");
        assert!(matches!(read_setting(&file), Err(GuiError::Setup { .. })));
        let env = env_of(&[]);
        let none = |_: &Path| false;
        let shown = setting_on(&file, &lookup(&env, Some("subl"), &none));
        assert_eq!(
            (shown.effective.as_str(), shown.effective_source),
            ("subl", EditorSource::Setting)
        );
    }
}
