//! The coding agent the notes are handed to (PERSO-315): what `[agent]`
//! names, and the words a hand-off starts it with. The commands that start
//! it are [`crate::handoff`]'s.
//!
//! `[agent]` of `config.toml` names the command (`default`, among the
//! built-in `pi` and `claude` and `[agent.commands]`), where it opens
//! (`open_in`) and the directory it starts in (`cwd`). The desktop app opens
//! the agent in its panel beside the reader whatever `open_in` says
//! ([`AgentRoute::Panel`]), except `clipboard`, which copies the batch prompt
//! and starts nothing, and `paseo`, a detached `paseo run`.

use std::path::{Path, PathBuf};

use mp_core::config::{AgentConfig, AgentOpenIn};
use serde::Serialize;

/// Where the desktop app opens the agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(rename_all = "snake_case")]
pub enum AgentRoute {
    /// The terminal panel beside the reader.
    Panel,
    /// No agent: the batch prompt is copied.
    Clipboard,
    /// A detached `paseo run`.
    Paseo,
}

/// Where `open_in` opens the agent in the desktop app: every place the TUI
/// opens a terminal in is the panel here, named one by one so a new
/// `open_in` value is a compile error until it is placed.
pub fn route(open_in: AgentOpenIn) -> AgentRoute {
    match open_in {
        AgentOpenIn::Clipboard => AgentRoute::Clipboard,
        AgentOpenIn::Paseo => AgentRoute::Paseo,
        AgentOpenIn::Auto
        | AgentOpenIn::HerdrApp
        | AgentOpenIn::Herdr
        | AgentOpenIn::Tmux
        | AgentOpenIn::Wezterm
        | AgentOpenIn::Terminal
        | AgentOpenIn::DesktopPanel => AgentRoute::Panel,
    }
}

/// The agent a hand-off starts, as `[agent]` names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct AgentSettings {
    pub route: AgentRoute,
    pub name: String,
    pub command: Vec<String>,
    pub cwd: String,
}

/// The settings `config` names: its route, its default command, not yet
/// substituted, and its directory with `~` read as `home`; the sentence of
/// a default that names no command.
pub fn settings(config: &AgentConfig, home: Option<&Path>) -> Result<AgentSettings, String> {
    let command = config.default_command().ok_or_else(|| {
        format!(
            "[agent] default names `{}`, which is neither built in nor in [agent.commands]",
            config.default
        )
    })?;
    if command
        .first()
        .is_none_or(|program| program.trim().is_empty())
    {
        return Err(format!(
            "the agent command `{}` names no program",
            config.default
        ));
    }
    Ok(AgentSettings {
        route: route(config.open_in),
        name: config.default.clone(),
        command,
        cwd: mp_core::draft::expand_home(&config.cwd, home)
            .to_string_lossy()
            .into_owned(),
    })
}

/// `command` with `{batch}` and `{date}` replaced in every word.
pub fn substitute(command: &[String], batch: &str, date: &str) -> Vec<String> {
    command
        .iter()
        .map(|word| word.replace("{batch}", batch).replace("{date}", date))
        .collect()
}

/// `date` when it is a `YYYY-MM-DD` date, as the webview names today.
pub fn checked_date(date: &str) -> Result<&str, String> {
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    let mut parts = date.split('-');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(y), Some(m), Some(d), None) if digits(y, 4) && digits(m, 2) && digits(d, 2) => {
            Ok(date)
        }
        _ => Err(format!("`{date}` is not a date of the form 2026-11-16")),
    }
}

/// The file a hand-off writes its prompt to: `<data>/notes/batch-<date>-<millis>.md`.
pub fn batch_path(data_dir: &Path, date: &str, millis: u128) -> PathBuf {
    data_dir
        .join("notes")
        .join(format!("batch-{date}-{millis}.md"))
}

/// Write `text` to [`batch_path`], its directory created private to the
/// user, and prune the batch files there older than
/// [`BATCH_KEPT_FOR`](mp_core::agent_batch::BATCH_KEPT_FOR), as the TUI does.
pub fn write_batch(
    data_dir: &Path,
    date: &str,
    millis: u128,
    text: &str,
) -> Result<PathBuf, String> {
    let path = batch_path(data_dir, date, millis);
    let dir = path.parent().unwrap_or(data_dir);
    mp_core::config::create_private_dir_all(dir)
        .map_err(|e| format!("could not create {}: {e:#}", dir.display()))?;
    std::fs::write(&path, text).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    mp_core::agent_batch::prune_batches(dir, std::time::SystemTime::now());
    Ok(path)
}

/// `paseo run`, detached, on the batch, the agent's name as its provider.
pub fn paseo_argv(name: &str, cwd: &str, batch: &str, date: &str) -> Vec<String> {
    vec![
        "paseo".to_string(),
        "run".to_string(),
        "--background".to_string(),
        "--provider".to_string(),
        name.to_string(),
        "--title".to_string(),
        format!("mail notes {date}"),
        "--cwd".to_string(),
        cwd.to_string(),
        format!("Work through the notes in {batch}."),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_desktop_opens_every_route_in_its_panel_but_clipboard_and_paseo() {
        for (open_in, expected) in [
            (AgentOpenIn::Auto, AgentRoute::Panel),
            (AgentOpenIn::HerdrApp, AgentRoute::Panel),
            (AgentOpenIn::Herdr, AgentRoute::Panel),
            (AgentOpenIn::Tmux, AgentRoute::Panel),
            (AgentOpenIn::Wezterm, AgentRoute::Panel),
            (AgentOpenIn::Terminal, AgentRoute::Panel),
            (AgentOpenIn::DesktopPanel, AgentRoute::Panel),
            (AgentOpenIn::Paseo, AgentRoute::Paseo),
            (AgentOpenIn::Clipboard, AgentRoute::Clipboard),
        ] {
            assert_eq!(route(open_in), expected, "{open_in:?}");
        }
    }

    #[test]
    fn the_built_in_settings_run_pi_in_pi_remote_under_home() {
        let s = settings(&AgentConfig::default(), Some(Path::new("/home/me"))).expect("settings");
        assert_eq!(s.route, AgentRoute::Panel);
        assert_eq!(s.name, "pi");
        assert_eq!(s.command[0], "pi");
        assert_eq!(s.cwd, "/home/me/pi-remote");
    }

    #[test]
    fn a_default_that_names_no_command_says_so() {
        let mut config = AgentConfig::default();
        config.default = "codex".to_string();
        let why = settings(&config, None).expect_err("refused");
        assert!(why.contains("`codex`"), "{why}");
    }

    #[test]
    fn batch_and_date_are_replaced_in_every_word() {
        let command = ["pi", "--name", "mail notes {date}", "@{batch}", "go"].map(String::from);
        assert_eq!(
            substitute(&command, "/d/notes/b.md", "2026-11-16"),
            [
                "pi",
                "--name",
                "mail notes 2026-11-16",
                "@/d/notes/b.md",
                "go"
            ]
        );
    }

    #[test]
    fn only_a_calendar_date_is_taken() {
        assert_eq!(checked_date("2026-11-16"), Ok("2026-11-16"));
        for bad in [
            "2026-11-6",
            "26-11-16",
            "2026/11/16",
            "2026-11-16-1",
            "../x",
            "",
        ] {
            assert!(checked_date(bad).is_err(), "{bad:?}");
        }
    }

    /// Writing a batch deletes the batch files older than a week, as the
    /// TUI's does, and keeps the recent ones and every file not named as a
    /// batch.
    #[test]
    fn writing_a_batch_prunes_the_batches_older_than_a_week() {
        let data = crate::test_support::scratch_dir("batch-prune");
        let notes = data.join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        let day = std::time::Duration::from_secs(24 * 60 * 60);
        let aged = |name: &str, days: u32| {
            let path = notes.join(name);
            std::fs::write(&path, "x").unwrap();
            let file = std::fs::File::options().write(true).open(&path).unwrap();
            file.set_modified(std::time::SystemTime::now() - day * days)
                .unwrap();
            path
        };
        let old = aged("batch-2026-11-01-1761987600000.md", 8);
        let recent = aged("batch-2026-11-12-1762938000000.md", 6);
        let other = aged("notes.json", 30);
        let lookalike = aged("batch-mine.md", 30);
        let written = write_batch(&data, "2026-11-16", 42, "I have notes").expect("written");
        assert_eq!(written, notes.join("batch-2026-11-16-42.md"));
        assert_eq!(std::fs::read_to_string(&written).unwrap(), "I have notes");
        assert!(!old.exists(), "the eight-day-old batch is pruned");
        assert!(recent.exists());
        assert!(other.exists() && lookalike.exists(), "only batch files go");
    }

    #[test]
    fn the_batch_goes_under_the_data_dirs_notes_and_paseo_runs_detached() {
        assert_eq!(
            batch_path(Path::new("/data"), "2026-11-16", 42),
            Path::new("/data/notes/batch-2026-11-16-42.md")
        );
        let argv = paseo_argv("pi", "/home/me/pi-remote", "/data/notes/b.md", "2026-11-16");
        assert_eq!(&argv[..3], ["paseo", "run", "--background"]);
        assert_eq!(
            argv.last().unwrap(),
            "Work through the notes in /data/notes/b.md."
        );
    }
}
