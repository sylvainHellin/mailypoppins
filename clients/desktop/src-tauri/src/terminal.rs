//! Embedded terminal sessions (M5, #0130): a terminal editor on a draft, in a
//! native PTY the webview renders with xterm.js.
//!
//! # Resolution
//!
//! [`editor::resolve_terminal_editor`] picks the template before any terminal
//! wrapping: `MP_DESKTOP_EDITOR`, the editor setting, then the first of
//! `$VISUAL` and `$EDITOR` naming a terminal editor ([`TERMINAL_EDITORS`]).
//! A GUI editor named there is a [`GuiError::Setup`] naming it; with nothing
//! named, [`TERMINAL_PROBES`] are probed in order. A bare program is located
//! on the login shell's `PATH` ([`login_env`]), then in [`PROBE_DIRS`], then
//! in [`BOB_DIR`] under `$HOME`, so a Finder launch finds Homebrew's or bob's
//! Neovim; a program found nowhere is a `setup` error too. [`route`] answers
//! whether that resolution succeeds, which `editor_setting_get` reports as
//! the route a draft takes, so the frontend picks this or `editor_open`.
//!
//! The child gets that `PATH`, `TERM=xterm-256color`, `COLORTERM=truecolor`
//! and, when the app has none of [`LOCALE_VARS`], the login shell's `LANG` or
//! [`DEFAULT_LANG`].
//!
//! # The look
//!
//! `terminal_spawn` takes the app's palette, `dark` or `light`, and
//! [`Launch::dressed`] puts it on the launch (#0137): `MP_DESKTOP_THEME` in
//! the child's environment always, and for Neovim and Vim ([`is_vim`]),
//! right after the program, `--cmd "set runtimepath^=<resources>/nvim"`,
//! `-c` of the same, then, while the `editor_colors` setting is `app`,
//! `-c "set background=<palette>" -c "colorscheme mailypoppins"`. `--cmd`
//! runs before the user's config, so the config can use the colorscheme;
//! `-c` runs after it and after the file loads, so the directory is back on
//! a runtime path the config reset (lazy.nvim resets it by default), and the
//! app's colours win over the config's colorscheme. The colorscheme is
//! `resources/nvim/colors/mailypoppins.vim`, shipped as the bundle's `nvim/`
//! resource ([`RESOURCE_SUBDIR`]).
//!
//! # The channel
//!
//! Each session has one `Channel<InvokeResponseBody>`. PTY output goes as
//! [`InvokeResponseBody::Raw`] frames (an `ArrayBuffer` in JavaScript),
//! coalesced by the pump thread to at most [`FRAME_MAX`] bytes or
//! [`FRAME_WINDOW`] after the first unsent byte. The last frame is the exit,
//! [`InvokeResponseBody::Json`] of a [`TerminalExitFrame`]:
//! `{"exit":{"code":0,"signal":null}}`. Tauri numbers every message of a
//! channel and the JavaScript `Channel` replays them in that order whatever
//! their body, so the exit never overtakes the last output.
//!
//! # Lifecycle
//!
//! A writer thread takes what `terminal_write` queues, so a child that stops
//! reading never blocks a command, and keystrokes keep their call order; a
//! failed write ends it and drops the rest, since it only fails once the child
//! closed the terminal. A reader thread does the blocking PTY reads; the pump
//! thread coalesces,
//! polls the child while the output is quiet, reaps it after EOF and sends the
//! exit frame. A child that exited while a grandchild still holds the PTY open
//! gets its exit frame after [`EXIT_GRACE`] of quiet. The session stays in
//! the table until `terminal_kill`, which the frontend calls after the exit
//! frame too; writes and resizes to an exited session are dropped.
//! Every live child is killed when the window is destroyed, when the app
//! exits, and when the table drops.
//!
//! In fixture mode nothing is spawned: the command is journaled as
//! `editor_open` journals it, no frame is sent, and `terminal_kill` sends the
//! exit frame `{code: 0, signal: null}`.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Manager, State};

use crate::editor::{
    self, command_line, live_lookup, quote, read_setting_or_none, settings_file, EditorRoute,
    EditorSource, Lookup, Resolved, EDITOR_ENV, PROBE_DIRS, TERMINAL_EDITORS,
};
use crate::error::GuiError;
use crate::fixture::Fixture;
use crate::session::SessionHandle;
use crate::settings::{editor_colors, EditorColors};

/// The largest output frame.
pub const FRAME_MAX: usize = 64 * 1024;

/// How long the first unsent byte waits for more.
pub const FRAME_WINDOW: Duration = Duration::from_millis(4);

/// How often a quiet pump asks whether the child exited.
const CHILD_POLL: Duration = Duration::from_millis(50);

/// How long the pump keeps reading after the child exited, while a
/// grandchild may still hold the PTY.
pub const EXIT_GRACE: Duration = Duration::from_millis(200);

/// How long the pump waits for a child after EOF before it kills it.
const REAP_PATIENCE: Duration = Duration::from_secs(2);

/// How long the login shell gets to print its `PATH`.
const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(5);

/// The terminal editors probed when nothing names one, in this order.
pub const TERMINAL_PROBES: &[&str] = &["nvim", "vim", "hx"];

/// bob's Neovim shims, under `$HOME`; probed after [`PROBE_DIRS`].
pub const BOB_DIR: &str = ".local/share/bob/nvim-bin";

/// The child's variable naming the app's palette, `dark` or `light`.
pub const THEME_ENV: &str = "MP_DESKTOP_THEME";

/// The runtime directory under the app's resource directory, holding
/// `colors/mailypoppins.vim`.
pub const RESOURCE_SUBDIR: &str = "nvim";

/// The colorscheme that runtime directory ships.
pub const COLORSCHEME: &str = "mailypoppins";

/// The program names that take `--cmd`, `-c` and the colorscheme.
pub const VIM_NAMES: &[&str] = &["nvim", "vim"];

/// A started session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct TerminalStarted {
    /// The id `terminal_write`, `terminal_resize` and `terminal_kill` take.
    pub session: u32,
    /// The editor's process; `null` in fixture mode.
    pub pid: Option<u32>,
    /// The command as it runs, quoted.
    pub editor: String,
    pub source: EditorSource,
    /// Fixture mode: the command was journaled and nothing was spawned.
    pub fixture: bool,
}

/// How the child ended: `code` when it exited, `signal` when a signal
/// killed it (Unix); both `null` when the status could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct TerminalExit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

/// The last frame of a session's channel, sent as JSON.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct TerminalExitFrame {
    pub exit: TerminalExit,
}

impl TerminalExit {
    fn json(self) -> String {
        serde_json::to_string(&TerminalExitFrame { exit: self })
            .unwrap_or_else(|_| r#"{"exit":{"code":null,"signal":null}}"#.to_string())
    }
}

/// What the pump hands its sink.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    Output(Vec<u8>),
    Exit(TerminalExit),
}

impl Frame {
    fn body(self) -> InvokeResponseBody {
        match self {
            Frame::Output(bytes) => InvokeResponseBody::Raw(bytes),
            Frame::Exit(exit) => InvokeResponseBody::Json(exit.json()),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

// ---------------------------------------------------------------------------
// Coalescing
// ---------------------------------------------------------------------------

/// The frame limits.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max: usize,
    pub window: Duration,
}

/// The production limits.
pub const LIMITS: Limits = Limits {
    max: FRAME_MAX,
    window: FRAME_WINDOW,
};

/// Output not yet sent, and when its first byte arrived.
#[derive(Debug)]
pub struct Coalescer {
    limits: Limits,
    buf: Vec<u8>,
    first: Option<Instant>,
}

impl Coalescer {
    pub fn new(limits: Limits) -> Coalescer {
        Coalescer {
            limits,
            buf: Vec::new(),
            first: None,
        }
    }

    /// Take `bytes` in, and answer every frame that reached the size limit.
    pub fn push(&mut self, bytes: &[u8], now: Instant) -> Vec<Vec<u8>> {
        if bytes.is_empty() {
            return Vec::new();
        }
        self.first.get_or_insert(now);
        self.buf.extend_from_slice(bytes);
        let mut full = Vec::new();
        while self.buf.len() >= self.limits.max {
            let rest = self.buf.split_off(self.limits.max);
            full.push(std::mem::replace(&mut self.buf, rest));
        }
        if self.buf.is_empty() {
            self.first = None;
        } else if !full.is_empty() {
            self.first = Some(now);
        }
        full
    }

    /// When the pending output is due, if there is any.
    pub fn deadline(&self) -> Option<Instant> {
        self.first.map(|t| t + self.limits.window)
    }

    /// The pending output, when its window has passed at `now`.
    pub fn poll(&mut self, now: Instant) -> Option<Vec<u8>> {
        match self.deadline() {
            Some(due) if now >= due => self.flush(),
            _ => None,
        }
    }

    /// The pending output, due or not.
    pub fn flush(&mut self) -> Option<Vec<u8>> {
        self.first = None;
        (!self.buf.is_empty()).then(|| std::mem::take(&mut self.buf))
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

/// Blocking reads off `reader` onto a channel, until EOF or an error (a PTY
/// master reads `EIO` on Linux once the slave side is gone).
pub fn spawn_reader(mut reader: Box<dyn Read + Send>) -> Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("mp-pty-reader".into())
        .spawn(move || {
            let mut buf = vec![0u8; FRAME_MAX];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        tracing::debug!("[terminal] the PTY read ended: {e}");
                        break;
                    }
                }
            }
        });
    if let Err(e) = spawned {
        tracing::error!("[terminal] could not start the PTY reader: {e}");
    }
    rx
}

/// The child as the pump sees it.
pub trait Reap {
    /// The exit status, once the child has exited (and is reaped).
    fn try_reap(&mut self) -> Option<TerminalExit>;
    /// Kill a child that outlived its output.
    fn kill(&mut self);
}

/// Coalesce `rx` into output frames, then reap the child and send the exit
/// frame last. Returns when the exit frame has gone.
pub fn pump(
    rx: Receiver<Vec<u8>>,
    limits: Limits,
    child: &mut dyn Reap,
    sink: &mut dyn FnMut(Frame),
) {
    let mut c = Coalescer::new(limits);
    let mut exit: Option<TerminalExit> = None;
    loop {
        let now = Instant::now();
        let timeout = match c.deadline() {
            Some(due) => due.saturating_duration_since(now),
            None if exit.is_some() => EXIT_GRACE,
            None => CHILD_POLL,
        };
        match rx.recv_timeout(timeout) {
            Ok(bytes) => {
                for frame in c.push(&bytes, Instant::now()) {
                    sink(Frame::Output(frame));
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if let Some(frame) = c.poll(Instant::now()) {
                    sink(Frame::Output(frame));
                } else if c.is_empty() {
                    if exit.is_some() {
                        tracing::debug!("[terminal] the child exited and the PTY stayed open");
                        break;
                    }
                    exit = child.try_reap();
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    if let Some(frame) = c.flush() {
        sink(Frame::Output(frame));
    }
    let exit = exit.unwrap_or_else(|| {
        let start = Instant::now();
        let mut killed = false;
        loop {
            if let Some(exit) = child.try_reap() {
                break exit;
            }
            if !killed && start.elapsed() >= REAP_PATIENCE {
                tracing::warn!(
                    "[terminal] the child closed its terminal but did not exit; killing it"
                );
                child.kill();
                killed = true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    sink(Frame::Exit(exit));
}

/// A thread writing what `terminal_write` queues to the PTY, so a child that
/// stops reading blocks this thread and never a command. It ends when the
/// queue's sender drops or a write fails (`EIO` once the child closed the
/// terminal), and drops what is still queued.
fn spawn_writer(
    id: u32,
    mut writer: Box<dyn Write + Send>,
) -> std::io::Result<(Sender<Vec<u8>>, JoinHandle<()>)> {
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let handle = std::thread::Builder::new()
        .name(format!("mp-pty-writer-{id}"))
        .spawn(move || {
            for bytes in rx {
                if let Err(e) = writer.write_all(&bytes).and_then(|()| writer.flush()) {
                    tracing::debug!("[terminal] session {id}: input dropped: {e}");
                    break;
                }
            }
        })?;
    Ok((tx, handle))
}

// ---------------------------------------------------------------------------
// The child
// ---------------------------------------------------------------------------

type BoxedChild = Box<dyn Child + Send + Sync>;

/// The status of a reaped child, or `None` while it runs.
fn try_wait(child: &mut BoxedChild) -> Option<TerminalExit> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        let plain: &mut dyn Child = &mut **child;
        if let Some(std_child) = plain.downcast_mut::<std::process::Child>() {
            return match std_child.try_wait() {
                Ok(Some(status)) => Some(TerminalExit {
                    code: status.code(),
                    signal: status.signal(),
                }),
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!("[terminal] could not wait for the child: {e}");
                    Some(TerminalExit {
                        code: None,
                        signal: None,
                    })
                }
            };
        }
    }
    match child.try_wait() {
        Ok(Some(status)) => Some(TerminalExit {
            code: status
                .signal()
                .is_none()
                .then(|| i32::try_from(status.exit_code()).unwrap_or(i32::MAX)),
            signal: None,
        }),
        Ok(None) => None,
        Err(e) => {
            tracing::warn!("[terminal] could not wait for the child: {e}");
            Some(TerminalExit {
                code: None,
                signal: None,
            })
        }
    }
}

/// Kill the child unless it is already reaped, so a recycled pid is never
/// signalled. `portable-pty` sends SIGHUP, waits up to 200 ms, then SIGKILL.
fn kill_child(child: &Mutex<BoxedChild>) {
    let mut child = lock(child);
    if try_wait(&mut child).is_some() {
        return;
    }
    if let Err(e) = child.kill() {
        tracing::warn!("[terminal] could not kill the child: {e}");
    }
}

struct SharedChild(Arc<Mutex<BoxedChild>>);

impl Reap for SharedChild {
    fn try_reap(&mut self) -> Option<TerminalExit> {
        try_wait(&mut lock(&self.0))
    }

    fn kill(&mut self) {
        kill_child(&self.0);
    }
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// What a session runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    /// The located program and its arguments, the draft path among them.
    pub argv: Vec<String>,
    /// The draft's directory.
    pub cwd: PathBuf,
    /// Set over the inherited environment.
    pub env: Vec<(String, String)>,
    pub source: EditorSource,
}

/// The app's palette, as `terminal_spawn` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    Dark,
    Light,
}

impl Scheme {
    /// `dark` or `light`; anything else is a `protocol` error.
    pub fn parse(theme: &str) -> Result<Scheme, GuiError> {
        match theme {
            "dark" => Ok(Scheme::Dark),
            "light" => Ok(Scheme::Light),
            other => Err(GuiError::protocol(format!(
                "the theme `{other}` is neither dark nor light"
            ))),
        }
    }

    /// The value of `MP_DESKTOP_THEME` and of Vim's `background`.
    pub fn name(self) -> &'static str {
        match self {
            Scheme::Dark => "dark",
            Scheme::Light => "light",
        }
    }
}

/// How a spawn dresses the editor ([`Launch::dressed`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Look {
    pub scheme: Scheme,
    pub colors: EditorColors,
    /// `<resources>/nvim`; `None` when the resource directory did not
    /// resolve, which leaves Neovim and Vim undressed.
    pub runtime: Option<PathBuf>,
}

/// Whether `program` is Neovim or Vim: its file name, or the file name of
/// what it links to (`/usr/bin/vi` is a link to `vim` on macOS), is one of
/// [`VIM_NAMES`].
pub fn is_vim(program: &str) -> bool {
    let named = |p: &Path| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| VIM_NAMES.contains(&n))
    };
    let path = Path::new(program);
    named(path) || std::fs::canonicalize(path).is_ok_and(|p| named(&p))
}

/// `value` as a `:set` value of a comma list such as 'runtimepath': a
/// backslash, a space, `|` and `"` escaped for `:set`, and a comma escaped
/// once more for the list.
fn set_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ',' => out.push_str("\\\\,"),
            ' ' | '|' | '"' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

impl Launch {
    /// The launch in `look`: see the module docs, "The look". Words go in
    /// right after the program, so they precede the user's arguments and
    /// the draft path.
    pub fn dressed(mut self, look: &Look) -> Launch {
        self.env
            .push((THEME_ENV.to_string(), look.scheme.name().to_string()));
        let Some(runtime) = &look.runtime else {
            return self;
        };
        if !self.argv.first().is_some_and(|p| is_vim(p)) {
            return self;
        }
        let rtp = format!("set runtimepath^={}", set_value(&runtime.to_string_lossy()));
        let mut words = vec!["--cmd".to_string(), rtp.clone(), "-c".to_string(), rtp];
        if look.colors == EditorColors::App {
            words.extend([
                "-c".to_string(),
                format!("set background={}", look.scheme.name()),
                "-c".to_string(),
                format!("colorscheme {COLORSCHEME}"),
            ]);
        }
        self.argv.splice(1..1, words);
        self
    }

    fn shown(&self) -> String {
        self.argv
            .iter()
            .map(|w| quote(w))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Where a bare program is looked for, in order: the login shell's `PATH`,
/// [`PROBE_DIRS`], then [`BOB_DIR`] under `home`.
pub fn search_dirs(login_path: &str, home: Option<&str>) -> Vec<String> {
    let mut dirs: Vec<String> = login_path
        .split(':')
        .filter(|d| !d.is_empty())
        .map(str::to_string)
        .collect();
    dirs.extend(PROBE_DIRS.iter().map(|d| d.to_string()));
    if let Some(home) = home.filter(|h| !h.is_empty()) {
        dirs.push(Path::new(home).join(BOB_DIR).to_string_lossy().into_owned());
    }
    dirs
}

/// `program` located: a path with a slash when it is a file, a bare name in
/// the first of `dirs` that has it.
fn find(program: &str, dirs: &[String], is_file: &dyn Fn(&Path) -> bool) -> Option<String> {
    if program.contains('/') {
        return is_file(Path::new(program)).then(|| program.to_string());
    }
    dirs.iter()
        .map(|d| Path::new(d).join(program))
        .find(|p| is_file(p))
        .map(|p| p.to_string_lossy().into_owned())
}

/// Where a template came from, in the words a user fixes it with.
fn origin(source: EditorSource) -> String {
    match source {
        EditorSource::Env => EDITOR_ENV.to_string(),
        EditorSource::Setting => "the editor setting".to_string(),
        EditorSource::Visual => "$VISUAL".to_string(),
        EditorSource::Editor => "$EDITOR".to_string(),
        _ => "the probe".to_string(),
    }
}

fn setup(message: String) -> GuiError {
    GuiError::Setup { message }
}

/// The searched places, for a message.
fn searched() -> String {
    format!(
        "the login shell's PATH, {} or ~/{BOB_DIR}",
        PROBE_DIRS.join(", ")
    )
}

/// The editor an embedded session runs on `path`, located; `fixture` keeps a
/// program found nowhere bare, and journals `nvim` when nothing is named or
/// found, so fixture runs work on a machine without one.
pub fn plan(
    lookup: &Lookup,
    login: &LoginEnv,
    path: &str,
    fixture: bool,
) -> Result<Launch, GuiError> {
    let login_path = login.path.as_str();
    let file = Path::new(path);
    if !file.is_absolute() {
        return Err(GuiError::protocol(format!(
            "`{path}` is not an absolute path"
        )));
    }
    if !file.is_file() {
        return Err(GuiError::not_found(format!("no file at {path}")));
    }
    let cwd = file
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/"));
    let (argv, source) = editor_argv(lookup, login, path, fixture)?;
    let mut env = vec![
        ("PATH".to_string(), login_path.to_string()),
        ("TERM".to_string(), "xterm-256color".to_string()),
        ("COLORTERM".to_string(), "truecolor".to_string()),
    ];
    if !LOCALE_VARS.iter().any(|v| lookup.var(v).is_some()) {
        let lang = login
            .lang
            .clone()
            .unwrap_or_else(|| DEFAULT_LANG.to_string());
        env.push(("LANG".to_string(), lang));
    }
    Ok(Launch {
        argv,
        cwd,
        env,
        source,
    })
}

/// Where a draft opens: [`EditorRoute::Embedded`] exactly when
/// [`plan`] would accept the editor for an existing draft, so the frontend
/// never starts an embedded session `terminal_spawn` then refuses. A GUI
/// editor, a terminal editor found nowhere, and nothing found at all keep the
/// external route of `editor_open`.
pub fn route(lookup: &Lookup, login: &LoginEnv, fixture: bool) -> EditorRoute {
    match editor_argv(lookup, login, ROUTE_PROBE_PATH, fixture) {
        Ok(_) => EditorRoute::Embedded,
        Err(e) => {
            tracing::debug!("[terminal] the external route: {e}");
            EditorRoute::External
        }
    }
}

/// The path [`route`] resolves against; only its shape matters.
const ROUTE_PROBE_PATH: &str = "/draft.md";

/// The located argv a session runs on `path`, and where its template came
/// from: the half of [`plan`] that does not look at the draft file.
fn editor_argv(
    lookup: &Lookup,
    login: &LoginEnv,
    path: &str,
    fixture: bool,
) -> Result<(Vec<String>, EditorSource), GuiError> {
    let home = lookup.var("HOME");
    let dirs = search_dirs(&login.path, home.as_deref());
    let resolved = match editor::resolve_terminal_editor(lookup) {
        Ok(Some(r)) => r,
        Ok(None) => probe(&dirs, lookup.is_file, fixture)?,
        Err(gui) => {
            return Err(setup(format!(
                "The embedded editor runs a terminal editor ({}), and {} names `{}`, which is not one; \
                 set the editor setting or $EDITOR to a terminal editor, e.g. \"nvim\", \
                 or use the external editor for `{}`.",
                TERMINAL_EDITORS[..4].join(", ") + ", ...",
                origin(gui.source),
                gui.template,
                gui.template,
            )))
        }
    };
    let mut argv = command_line(&resolved.template, path).map_err(|why| {
        setup(format!(
            "The editor from {} does not read: {why}; fix it.",
            origin(resolved.source)
        ))
    })?;
    match find(&argv[0], &dirs, lookup.is_file) {
        Some(at) => argv[0] = at,
        None if fixture => {}
        None => {
            return Err(setup(format!(
                "`{}` from {} was not found in {}; set the editor setting to its full path, e.g. \"/opt/homebrew/bin/{}\".",
                argv[0],
                origin(resolved.source),
                searched(),
                Path::new(&argv[0])
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| argv[0].clone()),
            )))
        }
    }
    Ok((argv, resolved.source))
}

/// Any of these set in the app's environment leaves the child's locale alone.
pub const LOCALE_VARS: &[&str] = &["LANG", "LC_ALL", "LC_CTYPE"];

/// The child's `LANG` when the app has no locale (a Finder launch, whose
/// launchd environment has none) and the login shell sets no `LANG` either:
/// without it `/usr/bin/vim` runs in latin1 and splits an umlaut.
pub const DEFAULT_LANG: &str = "en_US.UTF-8";

/// The first of [`TERMINAL_PROBES`] found, name by name.
fn probe(
    dirs: &[String],
    is_file: &dyn Fn(&Path) -> bool,
    fixture: bool,
) -> Result<Resolved, GuiError> {
    for name in TERMINAL_PROBES {
        if let Some(at) = find(name, dirs, is_file) {
            return Ok(Resolved {
                template: quote(&at),
                source: EditorSource::Probe,
            });
        }
    }
    if fixture {
        return Ok(Resolved {
            template: TERMINAL_PROBES[0].to_string(),
            source: EditorSource::Probe,
        });
    }
    Err(setup(format!(
        "No terminal editor was found: {} are not in {}; install Neovim, \
         or set the editor setting or $EDITOR to a terminal editor's full path.",
        TERMINAL_PROBES.join(", "),
        searched()
    )))
}

/// What the login shell says about the user's environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginEnv {
    /// `$PATH`.
    pub path: String,
    /// `$LANG`, when it is set and not blank.
    pub lang: Option<String>,
}

/// The line the login shell prints ahead of its answer, so whatever its
/// startup files print first is skipped.
const LOGIN_MARK: &str = "__mp_login_env__";

/// The script the login shell runs: the mark, `PATH`, then `LANG`, one per
/// line (fish's builtin `printf` reads `\n` too, and joins a quoted `PATH`
/// with colons).
const LOGIN_SCRIPT: &str = r#"printf '\n%s\n%s\n%s\n' __mp_login_env__ "$PATH" "$LANG""#;

/// The login environment read off the shell's output: the lines after the
/// last mark, with a `PATH` that holds a directory.
pub fn parse_login_env(output: &[u8]) -> Option<LoginEnv> {
    let text = String::from_utf8_lossy(output);
    let at = text.rfind(&format!("{LOGIN_MARK}\n"))?;
    let mut lines = text[at + LOGIN_MARK.len() + 1..].split('\n');
    let path = lines.next()?.trim_end_matches('\r').trim();
    if !path.contains('/') {
        return None;
    }
    let lang = lines
        .next()
        .map(|l| l.trim_end_matches('\r').trim())
        .filter(|l| !l.is_empty())
        .map(str::to_string);
    Some(LoginEnv {
        path: path.to_string(),
        lang,
    })
}

/// `shell -lc` printing `PATH` and `LANG`, within [`LOGIN_SHELL_TIMEOUT`].
pub fn login_shell_env(shell: &str) -> Result<LoginEnv, String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(shell)
        .args(["-lc", LOGIN_SCRIPT])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start {shell}: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        out
    });
    let deadline = Instant::now() + LOGIN_SHELL_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{shell} did not answer within {LOGIN_SHELL_TIMEOUT:?}"
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(format!("could not wait for {shell}: {e}")),
        }
    };
    let out = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(format!("{shell} exited with {status}"));
    }
    parse_login_env(&out).ok_or_else(|| format!("{shell} printed no PATH"))
}

/// The user's login-shell `PATH` and `LANG`, read once per process; the
/// process's own `PATH` and no `LANG` when the shell does not answer.
pub fn login_env() -> &'static LoginEnv {
    static ENV: OnceLock<LoginEnv> = OnceLock::new();
    ENV.get_or_init(|| {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "/bin/sh".to_string());
        match login_shell_env(&shell) {
            Ok(env) => {
                tracing::info!(
                    "[terminal] the login shell's PATH: {}; LANG: {:?}",
                    env.path,
                    env.lang
                );
                env
            }
            Err(why) => {
                tracing::warn!("[terminal] using the process PATH: {why}");
                LoginEnv {
                    path: std::env::var("PATH").unwrap_or_default(),
                    lang: None,
                }
            }
        }
    })
}

// ---------------------------------------------------------------------------
// The session table
// ---------------------------------------------------------------------------

/// The draft a session edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub account: String,
    pub id: String,
    pub path: String,
}

struct Pty {
    master: Mutex<Box<dyn MasterPty + Send>>,
    /// The writer thread's queue; the thread ends when this drops.
    input: Sender<Vec<u8>>,
    /// Never joined: a child that stopped reading can hold it in a write.
    #[cfg_attr(not(test), allow(dead_code))]
    writer: JoinHandle<()>,
    child: Arc<Mutex<BoxedChild>>,
    pump: Mutex<Option<JoinHandle<()>>>,
    exited: Arc<AtomicBool>,
}

enum Kind {
    Pty(Pty),
    Fixture(Channel<InvokeResponseBody>),
}

/// One embedded editor.
struct Session {
    draft: Draft,
    kind: Kind,
}

impl Session {
    /// Kill the child and, with `join`, wait until its exit frame has gone.
    fn end(&self, join: bool) {
        match &self.kind {
            Kind::Fixture(output) => {
                let exit = TerminalExit {
                    code: Some(0),
                    signal: None,
                };
                let _ = output.send(Frame::Exit(exit).body());
            }
            Kind::Pty(pty) => {
                kill_child(&pty.child);
                if join {
                    if let Some(handle) = lock(&pty.pump).take() {
                        let _ = handle.join();
                    }
                }
            }
        }
    }
}

#[derive(Default)]
struct Inner {
    next: AtomicU32,
    sessions: Mutex<HashMap<u32, Arc<Session>>>,
}

impl Inner {
    fn kill_all(&self) {
        let all: Vec<Arc<Session>> = lock(&self.sessions).drain().map(|(_, s)| s).collect();
        for s in all {
            tracing::info!(
                "[terminal] killing the editor of {}/{}",
                s.draft.account,
                s.draft.id
            );
            s.end(false);
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.kill_all();
    }
}

/// The live sessions, managed as Tauri state.
#[derive(Clone, Default)]
pub struct Terminals(Arc<Inner>);

fn pty_size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn pty_error(what: &str, e: impl std::fmt::Display) -> GuiError {
    GuiError::internal(format!("the terminal could not {what}: {e}"))
}

impl Terminals {
    fn get(&self, session: u32) -> Option<Arc<Session>> {
        lock(&self.0.sessions).get(&session).cloned()
    }

    fn unknown(session: u32) -> GuiError {
        GuiError::not_found(format!("no terminal session {session}"))
    }

    /// Start `launch` on a PTY of `cols` by `rows`, or, with a fixture,
    /// journal it and start nothing.
    pub fn start(
        &self,
        fixture: Option<&Fixture>,
        draft: Draft,
        launch: Launch,
        cols: u16,
        rows: u16,
        output: Channel<InvokeResponseBody>,
    ) -> Result<TerminalStarted, GuiError> {
        let id = self.0.next.fetch_add(1, Ordering::SeqCst) + 1;
        let editor = launch.shown();
        if let Some(fixture) = fixture {
            tracing::info!("[terminal] fixture: would run {editor}");
            fixture.record_editor(&draft.path, launch.argv.clone());
            let session = Session {
                draft,
                kind: Kind::Fixture(output),
            };
            lock(&self.0.sessions).insert(id, Arc::new(session));
            return Ok(TerminalStarted {
                session: id,
                pid: None,
                editor,
                source: launch.source,
                fixture: true,
            });
        }
        tracing::info!("[terminal] {editor}");
        let pair = portable_pty::native_pty_system()
            .openpty(pty_size(cols, rows))
            .map_err(|e| pty_error("open a PTY", e))?;
        let mut command = CommandBuilder::from_argv(launch.argv.iter().map(Into::into).collect());
        command.cwd(&launch.cwd);
        for (k, v) in &launch.env {
            command.env(k, v);
        }
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| setup(format!("The editor did not start: `{editor}`: {e}.")))?;
        // The slave stays open in the child only, so its exit ends the reads.
        drop(pair.slave);
        let pid = child.process_id();
        let child = Arc::new(Mutex::new(child));
        let started = (|| {
            let reader = pair
                .master
                .try_clone_reader()
                .map_err(|e| pty_error("read", e))?;
            let writer = pair
                .master
                .take_writer()
                .map_err(|e| pty_error("write", e))?;
            Ok::<_, GuiError>((reader, writer))
        })();
        let (reader, writer) = match started {
            Ok(rw) => rw,
            Err(e) => {
                kill_child(&child);
                return Err(e);
            }
        };
        let (input, writer) = match spawn_writer(id, writer) {
            Ok(w) => w,
            Err(e) => {
                kill_child(&child);
                return Err(pty_error("start its writer", e));
            }
        };
        let exited = Arc::new(AtomicBool::new(false));
        let rx = spawn_reader(reader);
        let mut reap = SharedChild(child.clone());
        let pump_exited = exited.clone();
        let pumped = std::thread::Builder::new()
            .name(format!("mp-pty-pump-{id}"))
            .spawn(move || {
                let mut sink = |frame: Frame| {
                    if let Frame::Exit(exit) = &frame {
                        tracing::info!("[terminal] session {id} exited: {exit:?}");
                        pump_exited.store(true, Ordering::SeqCst);
                    }
                    if let Err(e) = output.send(frame.body()) {
                        tracing::debug!("[terminal] session {id}: the channel is gone: {e}");
                    }
                };
                pump(rx, LIMITS, &mut reap, &mut sink);
            });
        let pump_handle = match pumped {
            Ok(h) => h,
            Err(e) => {
                kill_child(&child);
                return Err(pty_error("start its pump", e));
            }
        };
        let session = Session {
            draft,
            kind: Kind::Pty(Pty {
                master: Mutex::new(pair.master),
                input,
                writer,
                child,
                pump: Mutex::new(Some(pump_handle)),
                exited,
            }),
        };
        lock(&self.0.sessions).insert(id, Arc::new(session));
        Ok(TerminalStarted {
            session: id,
            pid,
            editor,
            source: launch.source,
            fixture: false,
        })
    }

    /// Queue `data`'s bytes for the editor, in call order, without waiting
    /// for them to be written; dropped once it exited or a write failed.
    pub fn write(&self, session: u32, data: &str) -> Result<(), GuiError> {
        let s = self.get(session).ok_or_else(|| Self::unknown(session))?;
        let Kind::Pty(pty) = &s.kind else {
            return Ok(());
        };
        if data.is_empty() || pty.exited.load(Ordering::SeqCst) {
            return Ok(());
        }
        if pty.input.send(data.as_bytes().to_vec()).is_err() {
            tracing::debug!("[terminal] session {session}: the writer is gone; input dropped");
        }
        Ok(())
    }

    /// The PTY's new size; dropped once the editor exited.
    pub fn resize(&self, session: u32, cols: u16, rows: u16) -> Result<(), GuiError> {
        let s = self.get(session).ok_or_else(|| Self::unknown(session))?;
        let Kind::Pty(pty) = &s.kind else {
            return Ok(());
        };
        if pty.exited.load(Ordering::SeqCst) {
            return Ok(());
        }
        let resized = lock(&pty.master).resize(pty_size(cols, rows));
        resized.map_err(|e| pty_error("resize", e))
    }

    /// Kill, reap and drop a session; its exit frame has gone when this
    /// returns. An unknown session is fine.
    pub fn kill(&self, session: u32) {
        let s = lock(&self.0.sessions).remove(&session);
        if let Some(s) = s {
            s.end(true);
        }
    }

    /// Kill every live child, without waiting for the exit frames.
    pub fn kill_all(&self) {
        self.0.kill_all();
    }
}

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

fn task_failed(e: impl std::fmt::Display) -> GuiError {
    GuiError::internal(format!("the command task failed: {e}"))
}

/// `<resources>/nvim`: the bundle's `Contents/Resources/nvim` on macOS, and
/// under `tauri dev` the build's target directory (`target/debug/nvim`),
/// where tauri-build copies the resources; `None`, logged, when it does not
/// resolve.
fn runtime_dir(app: &AppHandle) -> Option<PathBuf> {
    match app.path().resource_dir() {
        Ok(dir) => Some(dir.join(RESOURCE_SUBDIR)),
        Err(e) => {
            tracing::warn!("[terminal] no resource directory, so no colorscheme: {e}");
            None
        }
    }
}

/// Start the terminal editor on a draft in the app's palette `theme`
/// (`dark` or `light`); output and the exit come on `output`.
#[tauri::command(rename_all = "snake_case")]
#[allow(clippy::too_many_arguments)]
pub async fn terminal_spawn(
    app: AppHandle,
    daemon: State<'_, SessionHandle>,
    terminals: State<'_, Terminals>,
    account: String,
    id: String,
    path: String,
    cols: u16,
    rows: u16,
    theme: String,
    output: Channel<InvokeResponseBody>,
) -> Result<TerminalStarted, GuiError> {
    let file = settings_file(&app)?;
    let scheme = Scheme::parse(&theme)?;
    let runtime = runtime_dir(&app);
    let fixture = daemon.fixture();
    let terminals = terminals.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let lookup = live_lookup(read_setting_or_none(&file));
        let look = Look {
            scheme,
            colors: editor_colors(&file),
            runtime,
        };
        let launch = plan(&lookup, login_env(), &path, fixture.is_some())?.dressed(&look);
        let draft = Draft { account, id, path };
        terminals.start(fixture.as_deref(), draft, launch, cols, rows, output)
    })
    .await
    .map_err(task_failed)?
}

/// Keys typed in the terminal, as xterm's `onData` hands them over.
#[tauri::command(rename_all = "snake_case")]
pub async fn terminal_write(
    terminals: State<'_, Terminals>,
    session: u32,
    data: String,
) -> Result<(), GuiError> {
    terminals.write(session, &data)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn terminal_resize(
    terminals: State<'_, Terminals>,
    session: u32,
    cols: u16,
    rows: u16,
) -> Result<(), GuiError> {
    terminals.resize(session, cols, rows)
}

/// Kill the editor, reap it and drop the session; idempotent.
#[tauri::command(rename_all = "snake_case")]
pub async fn terminal_kill(terminals: State<'_, Terminals>, session: u32) -> Result<(), GuiError> {
    let terminals = terminals.inner().clone();
    tauri::async_runtime::spawn_blocking(move || terminals.kill(session))
        .await
        .map_err(task_failed)
}

#[cfg(test)]
mod tests;
