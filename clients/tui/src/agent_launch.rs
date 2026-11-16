//! The coding agent the notes are handed to (PERSO-315).
//!
//! The daemon renders the batch prompt (`note.export`) and marks the notes
//! it gave (`note.handoff`); starting the agent is the client's work, as
//! `[agent]` in `config.toml` says, because only the client knows the
//! terminal it runs in. The agent is a program of the user's, like `$EDITOR`
//! and `zoxide`, never an `mp` subprocess: it does its mail work through the
//! `mp` commands the batch prompt names.
//!
//! Everything that decides is pure and tested here: where the agent opens
//! ([`place`], from `open_in` and the environment, and [`settle`] once the
//! Herdr app was asked for this terminal's space), its command line
//! ([`agent_argv`], `{batch}` and `{date}` substituted) and what is run to
//! open it there ([`plan`], [`herdr_app_split`], [`hc_tab`]). The functions
//! that run something, [`run_to_end`], [`run_in_herdr`], [`run_in_herdr_app`]
//! and [`this_terminals_space`], start the multiplexer's own command and wait
//! for it to say the pane exists; the agent itself then runs in that pane,
//! detached from the TUI. Only [`Launch::Foreground`] runs the agent here,
//! the TUI suspended as for `$EDITOR`, which `actions.rs` does.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use mp_core::config::AgentOpenIn;

/// The environment variable herdr-core sets in every terminal it runs, so in
/// every pane of the Herdr app: the terminal's id, which `hc get` answers for.
pub const HC_TERMINAL_ID: &str = "HC_TERMINAL_ID";

/// The environment variable the Herdr TUI (herdr 0.9) sets to `1` in every
/// pane it runs.
pub const HERDR_ENV: &str = "HERDR_ENV";

/// What the launcher reads of the environment: which multiplexer, if any,
/// the TUI runs inside.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Env {
    /// In an hc terminal (`$HC_TERMINAL_ID` set), which the Herdr app shows
    /// when the terminal carries a `herdr.space` label.
    pub herdr_app: bool,
    /// Inside a pane of the Herdr TUI (`HERDR_ENV=1`).
    pub herdr: bool,
    /// Inside tmux (`$TMUX` set).
    pub tmux: bool,
    /// Inside a WezTerm pane (`$WEZTERM_PANE` set).
    pub wezterm: bool,
}

impl Env {
    /// The process's own environment.
    pub fn from_process() -> Self {
        let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
        Env {
            herdr_app: set(HC_TERMINAL_ID),
            herdr: std::env::var(HERDR_ENV).is_ok_and(|value| value == "1"),
            tmux: set("TMUX"),
            wezterm: set("WEZTERM_PANE"),
        }
    }
}

/// Where the agent opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// The Herdr app: a pane split to the right of the TUI's, or, on a
    /// machine the app shows over ssh, a new tab in the TUI's space.
    HerdrApp,
    /// A Herdr TUI pane split to the right of the TUI's.
    Herdr,
    /// A tmux pane split to the right.
    Tmux,
    /// A WezTerm pane split to the right.
    Wezterm,
    /// The TUI's own terminal, the TUI suspended until the agent exits.
    Terminal,
    /// A detached `paseo run`.
    Paseo,
    /// No agent: the batch prompt goes to the clipboard.
    Clipboard,
}

impl Place {
    /// The word the status line names the place with.
    pub fn word(self) -> &'static str {
        match self {
            Place::HerdrApp => "the Herdr app",
            Place::Herdr => "a Herdr pane",
            Place::Tmux => "a tmux pane",
            Place::Wezterm => "a WezTerm pane",
            Place::Terminal => "this terminal",
            Place::Paseo => "Paseo",
            Place::Clipboard => "the clipboard",
        }
    }
}

/// Where `open_in` opens the agent in `env`. `auto` takes the first that
/// applies of the Herdr app, the Herdr TUI, tmux and WezTerm, else this
/// terminal; the desktop's own panel is `auto` here, since the TUI has none.
pub fn place(open_in: AgentOpenIn, env: Env) -> Place {
    match open_in {
        AgentOpenIn::Auto | AgentOpenIn::DesktopPanel => {
            if env.herdr_app {
                Place::HerdrApp
            } else if env.herdr {
                Place::Herdr
            } else if env.tmux {
                Place::Tmux
            } else if env.wezterm {
                Place::Wezterm
            } else {
                Place::Terminal
            }
        }
        AgentOpenIn::HerdrApp => Place::HerdrApp,
        AgentOpenIn::Herdr => Place::Herdr,
        AgentOpenIn::Tmux => Place::Tmux,
        AgentOpenIn::Wezterm => Place::Wezterm,
        AgentOpenIn::Terminal => Place::Terminal,
        AgentOpenIn::Paseo => Place::Paseo,
        AgentOpenIn::Clipboard => Place::Clipboard,
    }
}

/// The place once the Herdr app was asked which space shows this terminal:
/// the app's own when `space` (this terminal's `herdr.space` label) is
/// known, else what `auto` takes after the app, since a terminal no space
/// shows (a hidden hc terminal, or none at all) has no pane or tab to open
/// beside. Any other place stands.
pub fn settle(place_first: Place, env: Env, space: Option<&str>) -> Place {
    if place_first == Place::HerdrApp && space.is_none() {
        place(
            AgentOpenIn::Auto,
            Env {
                herdr_app: false,
                ..env
            },
        )
    } else {
        place_first
    }
}

/// The configured agent command with `{batch}` (the batch file) and
/// `{date}` (today, `YYYY-MM-DD`) substituted in every word.
pub fn agent_argv(command: &[String], batch: &Path, date: &str) -> Vec<String> {
    let batch = batch.display().to_string();
    command
        .iter()
        .map(|word| word.replace("{batch}", &batch).replace("{date}", date))
        .collect()
}

/// What runs to open the agent in its place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// One program, run to its end: tmux and WezTerm split a pane running the
    /// agent and return, `paseo run --background` starts it and returns.
    Run(Vec<String>),
    /// `herdr pane split`, whose answer names the new pane, then
    /// `herdr pane run <pane> <line>`, which types the line into its shell.
    Herdr { split: Vec<String>, line: String },
    /// The agent in the Herdr app: [`herdr_app_split`], else, on a machine
    /// with no app, [`hc_tab`] ([`run_in_herdr_app`]).
    HerdrApp { argv: Vec<String>, cwd: PathBuf },
    /// The agent itself in `cwd`, in this terminal.
    Foreground { argv: Vec<String>, cwd: PathBuf },
    /// Nothing runs.
    Clipboard,
}

/// What opens `argv` in `place`, started in `cwd`. Paseo takes the agent by
/// its provider name (`[agent] default`) and the batch prompt itself as its
/// task, since it starts the agent its own way rather than by command line.
pub fn plan(
    place: Place,
    argv: Vec<String>,
    cwd: &Path,
    provider: &str,
    prompt: &str,
    date: &str,
) -> Launch {
    let cwd_text = cwd.display().to_string();
    let words = |words: &[&str]| -> Vec<String> { words.iter().map(|w| w.to_string()).collect() };
    match place {
        Place::HerdrApp => Launch::HerdrApp {
            argv,
            cwd: cwd.to_path_buf(),
        },
        Place::Herdr => Launch::Herdr {
            split: words(&[
                "herdr",
                "pane",
                "split",
                "--current",
                "--direction",
                "right",
                "--cwd",
                &cwd_text,
                "--focus",
            ]),
            line: shell_line(&argv),
        },
        Place::Tmux => {
            let mut run = words(&["tmux", "split-window", "-h", "-c", &cwd_text]);
            run.extend(argv);
            Launch::Run(run)
        }
        Place::Wezterm => {
            let mut run = words(&[
                "wezterm",
                "cli",
                "split-pane",
                "--right",
                "--cwd",
                &cwd_text,
                "--",
            ]);
            run.extend(argv);
            Launch::Run(run)
        }
        Place::Paseo => Launch::Run(words(&[
            "paseo",
            "run",
            "--background",
            "--provider",
            provider,
            "--cwd",
            &cwd_text,
            "--title",
            &format!("mail notes {date}"),
            prompt,
        ])),
        Place::Terminal => Launch::Foreground {
            argv,
            cwd: cwd.to_path_buf(),
        },
        Place::Clipboard => Launch::Clipboard,
    }
}

/// `herdr-app do pane.split`, which splits the app's focused pane, the TUI's
/// when `x` was pressed, to the right with `argv` in it. `argv` runs through
/// `sh` to start in `cwd`, since `pane.split` takes no directory.
pub fn herdr_app_split(argv: &[String], cwd: &Path) -> Vec<String> {
    let mut run = vec![
        "sh".to_string(),
        "-c".to_string(),
        r#"cd "$1" && shift && exec "$@""#.to_string(),
        "sh".to_string(),
        cwd.display().to_string(),
    ];
    run.extend(argv.iter().cloned());
    let args = serde_json::json!({ "direction": "right", "argv": run });
    vec![
        "herdr-app".to_string(),
        "do".to_string(),
        "pane.split".to_string(),
        args.to_string(),
    ]
}

/// `hc run` starting `argv` in `cwd` as a terminal labelled with app space
/// `space` and the fresh tab id `tab`, which the Herdr app adopts as a new
/// tab of that space: the route on a machine the app shows over ssh, which
/// has no `herdr-app`. The terminal has no parent, so it outlives the TUI,
/// and `argv` runs under `path` (the TUI's `PATH`), since the core's own
/// environment may lack the shell's.
pub fn hc_tab(argv: &[String], cwd: &Path, space: &str, tab: &str, path: &str) -> Vec<String> {
    let mut run: Vec<String> = [
        "hc",
        "run",
        "--bg",
        "--json",
        "--no-parent",
        "--keep",
        "--cwd",
    ]
    .iter()
    .map(|w| w.to_string())
    .collect();
    run.push(cwd.display().to_string());
    run.extend([
        "--label".to_string(),
        format!("herdr.space={space}"),
        "--label".to_string(),
        format!("herdr.tab={tab}"),
        "--".to_string(),
        "env".to_string(),
        format!("PATH={path}"),
    ]);
    run.extend(argv.iter().cloned());
    run
}

/// `hc get <terminal> --json`, whose labels say where the app shows it.
pub fn hc_get(terminal: &str) -> Vec<String> {
    ["hc", "get", terminal, "--json"]
        .iter()
        .map(|w| w.to_string())
        .collect()
}

/// The `herdr.space` label out of `hc get`'s JSON answer, `None` when the
/// terminal has none (no app space shows it).
pub fn herdr_space(answer: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(answer.trim()).ok()?;
    value["labels"]["herdr.space"]
        .as_str()
        .filter(|space| !space.is_empty())
        .map(str::to_string)
}

/// A new app tab id: `tb` and 32 hex digits, the form the app gives its own.
pub fn tab_id(random: [u8; 16]) -> String {
    let hex: String = random.iter().map(|b| format!("{b:02x}")).collect();
    format!("tb{hex}")
}

/// Whether a failed `herdr-app` call means this machine has no app: the
/// program is not installed, or no app window runs.
pub fn no_app_here(failure: &RunFailure) -> bool {
    failure.missing || failure.message.contains("no Herdr window")
}

/// `argv` as one line a POSIX shell or fish reads back as the same words:
/// each word single-quoted, a quote inside one closed, escaped and reopened.
pub fn shell_line(argv: &[String]) -> String {
    argv.iter()
        .map(|word| {
            let plain = !word.is_empty()
                && word
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./@:=+,".contains(c));
            if plain {
                word.clone()
            } else {
                format!("'{}'", word.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The agent's working directory: `[agent] cwd` with `~` expanded.
pub fn agent_cwd(configured: &str) -> PathBuf {
    PathBuf::from(shellexpand::tilde(configured).into_owned())
}

/// The batch file of one hand-off under `dir`, named by its moment so a
/// second hand-off never rewrites the file an agent may still be reading.
pub fn batch_path(dir: &Path, stamp: &str) -> PathBuf {
    dir.join(format!("batch-{stamp}.md"))
}

/// Write the batch prompt to [`batch_path`], creating `dir`.
pub fn write_batch(dir: &Path, stamp: &str, text: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let path = batch_path(dir, stamp);
    std::fs::write(&path, text).map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(path)
}

/// Where the batch files go: `<data_dir>/notes/`, beside the daemon's
/// `notes.json`.
pub fn batch_dir() -> PathBuf {
    mp_core::config::mailypoppins_data_dir().join("notes")
}

/// Why a command did not run to a clean end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunFailure {
    /// The program is not installed (`ENOENT` on the spawn).
    pub missing: bool,
    /// What went wrong, with what the program printed to stderr.
    pub message: String,
}

/// Run `argv` to its end, its output captured, telling a missing program
/// from one that failed.
pub fn run(argv: &[String]) -> Result<String, RunFailure> {
    let failure = |missing, message| RunFailure { missing, message };
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| failure(false, "an empty command".to_string()))?;
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| {
            failure(
                e.kind() == std::io::ErrorKind::NotFound,
                format!("{program}: {e}"),
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(failure(
            false,
            format!("{program} exited with {}: {}", output.status, stderr.trim()),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Run `argv` to its end, its output captured: what it printed to stderr
/// is the error when it fails.
pub fn run_to_end(argv: &[String]) -> Result<String, String> {
    run(argv).map_err(|failure| failure.message)
}

/// The app space showing this terminal, from its `herdr.space` label; `None`
/// outside an hc terminal, in one no space shows, or when `hc` fails.
pub fn this_terminals_space() -> Option<String> {
    let terminal = std::env::var(HC_TERMINAL_ID).ok()?;
    herdr_space(&run_to_end(&hc_get(&terminal)).ok()?)
}

/// Open `argv` in `cwd` in the Herdr app showing app space `space`: a pane
/// split right of the TUI's through `herdr-app`, or, where this machine has
/// no app (the home server, which the Mac's app shows over ssh), a new tab of
/// `space` started with `hc run`. The tab keeps the app's own number for a
/// name: naming it takes the core document's `doc.get` and `doc.set`.
pub fn run_in_herdr_app(argv: &[String], cwd: &Path, space: &str) -> Result<(), String> {
    match run(&herdr_app_split(argv, cwd)) {
        Ok(_) => Ok(()),
        Err(failure) if no_app_here(&failure) => {
            let path = std::env::var("PATH").unwrap_or_default();
            run_to_end(&hc_tab(argv, cwd, space, &tab_id(random_bytes()), &path)).map(|_| ())
        }
        Err(failure) => Err(failure.message),
    }
}

/// Sixteen random bytes from `/dev/urandom`, else from the standard
/// library's per-process hash keys mixed with the time, which keeps a tab id
/// unique if never secret.
fn random_bytes() -> [u8; 16] {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    let read = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes));
    if read.is_err() {
        use std::hash::{BuildHasher, Hasher};
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        for half in bytes.chunks_mut(8) {
            let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
            hasher.write_u128(nanos);
            hasher.write_u32(std::process::id());
            half.copy_from_slice(&hasher.finish().to_le_bytes());
        }
    }
    bytes
}

/// The new pane's id out of `herdr pane split`'s JSON answer,
/// `.result.pane.pane_id`.
pub fn herdr_pane_id(answer: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(answer.trim()).ok()?;
    value["result"]["pane"]["pane_id"]
        .as_str()
        .map(str::to_string)
}

/// Split a Herdr pane and run `line` in it.
pub fn run_in_herdr(split: &[String], line: &str) -> Result<(), String> {
    let answer = run_to_end(split)?;
    let pane = herdr_pane_id(&answer)
        .ok_or_else(|| format!("herdr pane split named no pane: {}", answer.trim()))?;
    run_to_end(&[
        "herdr".to_string(),
        "pane".to_string(),
        "run".to_string(),
        pane,
        line.to_string(),
    ])
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    const NONE: Env = Env {
        herdr_app: false,
        herdr: false,
        tmux: false,
        wezterm: false,
    };

    /// `auto` (and the desktop panel, which the TUI lacks) takes the Herdr
    /// app, then the Herdr TUI, then tmux, then WezTerm, then this terminal;
    /// a named place is that place.
    #[test]
    fn auto_takes_the_first_multiplexer_the_tui_runs_in() {
        let all = Env {
            herdr_app: true,
            herdr: true,
            tmux: true,
            wezterm: true,
        };
        for open_in in [AgentOpenIn::Auto, AgentOpenIn::DesktopPanel] {
            assert_eq!(place(open_in, all), Place::HerdrApp);
            assert_eq!(
                place(
                    open_in,
                    Env {
                        herdr_app: false,
                        ..all
                    }
                ),
                Place::Herdr
            );
            assert_eq!(
                place(
                    open_in,
                    Env {
                        herdr_app: false,
                        herdr: false,
                        ..all
                    }
                ),
                Place::Tmux
            );
            assert_eq!(
                place(
                    open_in,
                    Env {
                        wezterm: true,
                        ..NONE
                    }
                ),
                Place::Wezterm
            );
            assert_eq!(place(open_in, NONE), Place::Terminal);
        }
        assert_eq!(place(AgentOpenIn::Tmux, NONE), Place::Tmux);
        assert_eq!(place(AgentOpenIn::HerdrApp, NONE), Place::HerdrApp);
        assert_eq!(place(AgentOpenIn::Herdr, all), Place::Herdr);
        assert_eq!(place(AgentOpenIn::Terminal, all), Place::Terminal);
        assert_eq!(place(AgentOpenIn::Paseo, all), Place::Paseo);
        assert_eq!(place(AgentOpenIn::Clipboard, all), Place::Clipboard);
    }

    /// The Herdr app holds only for a terminal an app space shows; without a
    /// `herdr.space` label `auto`'s next option takes over, whether the app
    /// was detected or named, and any other place stands.
    #[test]
    fn the_herdr_app_without_a_space_falls_back_to_the_next_option() {
        let app_and_tmux = Env {
            herdr_app: true,
            tmux: true,
            ..NONE
        };
        let auto = place(AgentOpenIn::Auto, app_and_tmux);
        assert_eq!(settle(auto, app_and_tmux, Some("sp1")), Place::HerdrApp);
        assert_eq!(settle(auto, app_and_tmux, None), Place::Tmux);
        let app_and_herdr = Env {
            herdr_app: true,
            herdr: true,
            wezterm: true,
            ..NONE
        };
        assert_eq!(settle(Place::HerdrApp, app_and_herdr, None), Place::Herdr);
        let app_only = Env {
            herdr_app: true,
            ..NONE
        };
        let named = place(AgentOpenIn::HerdrApp, app_only);
        assert_eq!(settle(named, app_only, None), Place::Terminal);
        assert_eq!(settle(Place::Paseo, app_only, None), Place::Paseo);
        assert_eq!(settle(Place::Tmux, NONE, Some("sp1")), Place::Tmux);
    }

    /// On a machine with the app, `herdr-app do pane.split` opens the agent
    /// right of the TUI, started in its directory through `sh`; on one
    /// without, `hc run` opens it as a new tab of the TUI's space, under the
    /// TUI's PATH.
    #[test]
    fn the_herdr_app_splits_a_pane_or_opens_a_tab_of_the_space() {
        let argv = words(&["pi", "--name", "mail notes 2026-11-16", "@/b.md"]);
        let cwd = Path::new("/home/me/pi remote");
        assert_eq!(
            plan(Place::HerdrApp, argv.clone(), cwd, "pi", "PROMPT", "d"),
            Launch::HerdrApp {
                argv: argv.clone(),
                cwd: cwd.to_path_buf()
            }
        );

        let split = herdr_app_split(&argv, cwd);
        assert_eq!(split[..3], words(&["herdr-app", "do", "pane.split"]));
        assert_eq!(split.len(), 4);
        let args: serde_json::Value = serde_json::from_str(&split[3]).expect("one JSON object");
        assert_eq!(
            args,
            serde_json::json!({
                "direction": "right",
                "argv": [
                    "sh", "-c", "cd \"$1\" && shift && exec \"$@\"", "sh",
                    "/home/me/pi remote",
                    "pi", "--name", "mail notes 2026-11-16", "@/b.md",
                ],
            })
        );

        assert_eq!(
            hc_tab(&argv, cwd, "sp7", "tb00ff", "/usr/bin:/bin"),
            words(&[
                "hc",
                "run",
                "--bg",
                "--json",
                "--no-parent",
                "--keep",
                "--cwd",
                "/home/me/pi remote",
                "--label",
                "herdr.space=sp7",
                "--label",
                "herdr.tab=tb00ff",
                "--",
                "env",
                "PATH=/usr/bin:/bin",
                "pi",
                "--name",
                "mail notes 2026-11-16",
                "@/b.md",
            ])
        );
        assert_eq!(hc_get("t42"), words(&["hc", "get", "t42", "--json"]));
        assert_eq!(
            tab_id([0xab; 16]),
            format!("tb{}", "ab".repeat(16)),
            "tb and 32 hex digits"
        );
    }

    /// The space comes from `hc get`'s labels; no label, an empty one or no
    /// JSON is no space.
    #[test]
    fn hc_get_names_the_space_showing_the_terminal() {
        assert_eq!(
            herdr_space(r#"{"id":"t1","labels":{"herdr.space":"sp3","herdr.tab":"tb1"}}"#),
            Some("sp3".to_string())
        );
        assert_eq!(herdr_space(r#"{"id":"t1","labels":{}}"#), None);
        assert_eq!(herdr_space(r#"{"labels":{"herdr.space":""}}"#), None);
        assert_eq!(herdr_space("hc: no such terminal"), None);
    }

    /// A missing `herdr-app` or one with no window means no app here, which
    /// sends the agent to an hc tab; any other failure is the error.
    #[test]
    fn no_app_is_a_missing_program_or_no_window() {
        let failure = |missing: bool, message: &str| RunFailure {
            missing,
            message: message.to_string(),
        };
        assert!(no_app_here(&failure(true, "herdr-app: No such file")));
        assert!(no_app_here(&failure(
            false,
            "herdr-app exited with exit status: 1: no Herdr window is running"
        )));
        assert!(!no_app_here(&failure(
            false,
            "herdr-app exited with exit status: 2: unknown argument folder"
        )));
        let missing = run(&words(&["mp-no-such-program-315"])).unwrap_err();
        assert!(missing.missing, "{missing:?}");
        let failed = run(&words(&["sh", "-c", "exit 1"])).unwrap_err();
        assert!(!failed.missing, "{failed:?}");
    }

    /// `{batch}` and `{date}` are substituted inside words, the built-in pi
    /// command's `@{batch}` included.
    #[test]
    fn the_command_takes_the_batch_file_and_the_date() {
        let pi = mp_core::config::AgentConfig::default()
            .default_command()
            .expect("the built-in pi command");
        assert_eq!(
            agent_argv(&pi, Path::new("/d/notes/batch-1.md"), "2026-11-16"),
            words(&[
                "pi",
                "--name",
                "mail notes 2026-11-16",
                "@/d/notes/batch-1.md",
                "Work through these notes.",
            ])
        );
    }

    /// The command line each place runs, from the same agent words.
    #[test]
    fn each_place_builds_its_own_command_line() {
        let argv = words(&["pi", "--name", "mail notes 2026-11-16", "@/b.md"]);
        let cwd = Path::new("/home/me/pi-remote");
        let at = |place| plan(place, argv.clone(), cwd, "pi", "PROMPT", "2026-11-16");
        assert_eq!(
            at(Place::Herdr),
            Launch::Herdr {
                split: words(&[
                    "herdr",
                    "pane",
                    "split",
                    "--current",
                    "--direction",
                    "right",
                    "--cwd",
                    "/home/me/pi-remote",
                    "--focus",
                ]),
                line: "pi --name 'mail notes 2026-11-16' @/b.md".to_string(),
            }
        );
        assert_eq!(
            at(Place::Tmux),
            Launch::Run(words(&[
                "tmux",
                "split-window",
                "-h",
                "-c",
                "/home/me/pi-remote",
                "pi",
                "--name",
                "mail notes 2026-11-16",
                "@/b.md",
            ]))
        );
        assert_eq!(
            at(Place::Wezterm),
            Launch::Run(words(&[
                "wezterm",
                "cli",
                "split-pane",
                "--right",
                "--cwd",
                "/home/me/pi-remote",
                "--",
                "pi",
                "--name",
                "mail notes 2026-11-16",
                "@/b.md",
            ]))
        );
        assert_eq!(
            at(Place::Paseo),
            Launch::Run(words(&[
                "paseo",
                "run",
                "--background",
                "--provider",
                "pi",
                "--cwd",
                "/home/me/pi-remote",
                "--title",
                "mail notes 2026-11-16",
                "PROMPT",
            ]))
        );
        assert_eq!(
            at(Place::Terminal),
            Launch::Foreground {
                argv: argv.clone(),
                cwd: cwd.to_path_buf()
            }
        );
        assert_eq!(at(Place::Clipboard), Launch::Clipboard);
    }

    /// A word with a space or a quote survives the shell line Herdr types.
    #[test]
    fn the_shell_line_quotes_what_needs_it() {
        assert_eq!(
            shell_line(&words(&[
                "claude",
                "Work through the notes in /b.md.",
                "it's",
                ""
            ])),
            "claude 'Work through the notes in /b.md.' 'it'\\''s' ''"
        );
    }

    /// Herdr's split answer names the new pane.
    #[test]
    fn the_herdr_split_answer_names_the_pane() {
        assert_eq!(
            herdr_pane_id(r#"{"id":"1","result":{"pane":{"pane_id":"w1:t2:p3"}}}"#),
            Some("w1:t2:p3".to_string())
        );
        assert_eq!(herdr_pane_id("not json"), None);
    }

    /// The batch file lands in its directory under its moment's name, the
    /// directory created; `~` in the agent's directory is the home.
    #[test]
    fn the_batch_file_is_written_under_its_moment() {
        let dir = tempfile::tempdir().expect("a scratch dir");
        let notes = dir.path().join("notes");
        let path = write_batch(&notes, "20261116-101500", "I have notes").expect("written");
        assert_eq!(path, notes.join("batch-20261116-101500.md"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "I have notes");
        let home = PathBuf::from(shellexpand::tilde("~").into_owned());
        assert_ne!(home, PathBuf::from("~"), "a home to expand to");
        assert_eq!(agent_cwd("~/pi-remote"), home.join("pi-remote"));
        assert_eq!(agent_cwd("/abs"), PathBuf::from("/abs"));
    }

    /// A command that fails says what it printed; one that runs answers its
    /// output.
    #[test]
    fn a_failed_launch_command_names_its_error() {
        assert_eq!(run_to_end(&words(&["echo", "hi"])).unwrap(), "hi\n");
        let err = run_to_end(&words(&["sh", "-c", "echo nope >&2; exit 3"])).unwrap_err();
        assert!(err.contains("nope"), "{err}");
        assert!(run_to_end(&words(&["mp-no-such-program-315"])).is_err());
        assert!(run_to_end(&[]).is_err());
    }
}
