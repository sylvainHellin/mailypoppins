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
//! ([`place`], from `open_in` and the environment), its command line
//! ([`agent_argv`], `{batch}` and `{date}` substituted) and what is run to
//! open it there ([`plan`]). The two functions that run something,
//! [`run_to_end`] and [`run_in_herdr`], start the multiplexer's own command
//! and wait for it to say the pane exists; the agent itself then runs in that
//! pane, detached from the TUI. Only [`Launch::Foreground`] runs the agent
//! here, the TUI suspended as for `$EDITOR`, which `actions.rs` does.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use mp_core::config::AgentOpenIn;

/// The environment variable Herdr sets to `1` in every pane it runs.
pub const HERDR_ENV: &str = "HERDR_ENV";

/// What the launcher reads of the environment: which multiplexer, if any,
/// the TUI runs inside.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Env {
    /// Inside a Herdr pane (`HERDR_ENV=1`).
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
            herdr: std::env::var(HERDR_ENV).is_ok_and(|value| value == "1"),
            tmux: set("TMUX"),
            wezterm: set("WEZTERM_PANE"),
        }
    }
}

/// Where the agent opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// A Herdr pane split to the right of the TUI's.
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
/// applies of Herdr, tmux and WezTerm, else this terminal; the desktop's
/// own panel is `auto` here, since the TUI has none.
pub fn place(open_in: AgentOpenIn, env: Env) -> Place {
    match open_in {
        AgentOpenIn::Auto | AgentOpenIn::DesktopPanel => {
            if env.herdr {
                Place::Herdr
            } else if env.tmux {
                Place::Tmux
            } else if env.wezterm {
                Place::Wezterm
            } else {
                Place::Terminal
            }
        }
        AgentOpenIn::Herdr => Place::Herdr,
        AgentOpenIn::Tmux => Place::Tmux,
        AgentOpenIn::Wezterm => Place::Wezterm,
        AgentOpenIn::Terminal => Place::Terminal,
        AgentOpenIn::Paseo => Place::Paseo,
        AgentOpenIn::Clipboard => Place::Clipboard,
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

/// Run `argv` to its end, its output captured: what it printed to stderr
/// is the error when it fails.
pub fn run_to_end(argv: &[String]) -> Result<String, String> {
    let (program, args) = argv.split_first().ok_or("an empty command")?;
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{program} exited with {}: {}",
            output.status,
            stderr.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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
        herdr: false,
        tmux: false,
        wezterm: false,
    };

    /// `auto` (and the desktop panel, which the TUI lacks) takes Herdr, then
    /// tmux, then WezTerm, then this terminal; a named place is that place.
    #[test]
    fn auto_takes_the_first_multiplexer_the_tui_runs_in() {
        let all = Env {
            herdr: true,
            tmux: true,
            wezterm: true,
        };
        for open_in in [AgentOpenIn::Auto, AgentOpenIn::DesktopPanel] {
            assert_eq!(place(open_in, all), Place::Herdr);
            assert_eq!(
                place(
                    open_in,
                    Env {
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
        assert_eq!(place(AgentOpenIn::Herdr, NONE), Place::Herdr);
        assert_eq!(place(AgentOpenIn::Terminal, all), Place::Terminal);
        assert_eq!(place(AgentOpenIn::Paseo, all), Place::Paseo);
        assert_eq!(place(AgentOpenIn::Clipboard, all), Place::Clipboard);
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
