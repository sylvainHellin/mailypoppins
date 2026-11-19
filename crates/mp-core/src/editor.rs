//! The editor every client hands a file to (PERSO-320): which of `$VISUAL`
//! and `$EDITOR` names it, how an editor value splits into a program and its
//! arguments, and which editors need a terminal.
//!
//! One resolver for the three clients. The CLI (`mp edit`, `mp signature
//! edit`, `mp config open`) and the TUI run [`template`] in their own
//! terminal through [`command`]; the desktop app
//! (`clients/desktop/src-tauri/src/editor.rs`) puts its own choices before
//! these two variables (`MP_DESKTOP_EDITOR`, its setting), takes [`pick`] for
//! its embedded terminal and [`split`], [`quote`], [`command_line`] and
//! [`needs_terminal`] for every launch, and keeps to itself how an editor is
//! placed in a terminal emulator or a window.
//!
//! # The rule
//!
//! Of `$VISUAL` and `$EDITOR`, a blank one counting as unset, the first that
//! names a terminal editor ([`needs_terminal`]) wins; when neither does,
//! the first set one, a GUI editor, is taken as written; when neither is set,
//! [`FALLBACK`]. So `VISUAL="code -w"` beside `EDITOR=nvim` runs `nvim` in the
//! terminal, which the desktop's embedded terminal has always done, and
//! `VISUAL="code -w"` alone runs `code` with `-w`. A GUI editor without its
//! wait flag (`code` rather than `code -w`) returns at once, which a caller
//! that compares the file afterwards reads as no change.
//!
//! # Splitting
//!
//! A value is split the way a POSIX shell splits words, with no expansion and
//! no shell. `{path}` in any word is replaced by the file's path; a value
//! without it gets the path as its last argument.

use std::ffi::OsString;
use std::path::Path;

/// What runs when neither variable names an editor.
pub const FALLBACK: &str = "hx";

/// Editors that need a terminal, known by their program's file name. Emacs
/// and emacsclient are not on it: they need one only with a terminal flag
/// ([`EMACS_TERMINAL_FLAGS`]).
pub const TERMINAL_EDITORS: &[&str] = &[
    "vi", "vim", "nvim", "hx", "helix", "nano", "pico", "micro", "kak", "joe", "ne", "mg", "ed",
    "lvim", "nvi", "elvis", "vile", "vis", "jed", "zile", "mcedit", "amp", "dte", "ee", "jmacs",
    "jpico", "jstar", "tilde", "ox",
];

/// The programs that open a window unless one of [`EMACS_TERMINAL_FLAGS`]
/// keeps them in the terminal.
pub const EMACS_PROGRAMS: &[&str] = &["emacs", "emacsclient"];

/// The flags that run [`EMACS_PROGRAMS`] in the terminal.
pub const EMACS_TERMINAL_FLAGS: &[&str] = &["-nw", "--no-window-system", "-t", "--tty"];

/// Which variable named the editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvVar {
    /// `$VISUAL`.
    Visual,
    /// `$EDITOR`.
    Editor,
}

/// The rule of the module docs over the two values, before the fallback:
/// `Ok(Some)` is a terminal editor, `Ok(None)` means neither is set, and
/// `Err` is the first set one when neither names a terminal editor.
pub fn pick(
    visual: Option<&str>,
    editor: Option<&str>,
) -> Result<Option<(EnvVar, String)>, (EnvVar, String)> {
    let mut gui = None;
    for (var, value) in [(EnvVar::Visual, visual), (EnvVar::Editor, editor)] {
        let Some(value) = value.filter(|v| !v.trim().is_empty()) else {
            continue;
        };
        if needs_terminal(value) {
            return Ok(Some((var, value.to_string())));
        }
        gui.get_or_insert((var, value.to_string()));
    }
    gui.map_or(Ok(None), Err)
}

/// The editor a terminal client runs: [`pick`] over the two values, a GUI
/// editor taken when no terminal editor is named, else [`FALLBACK`].
pub fn template_from(visual: Option<&str>, editor: Option<&str>) -> String {
    match pick(visual, editor) {
        Ok(Some((_, value))) | Err((_, value)) => value,
        Ok(None) => FALLBACK.to_string(),
    }
}

/// [`template_from`] over this process's `$VISUAL` and `$EDITOR`.
pub fn template() -> String {
    template_from(
        std::env::var("VISUAL").ok().as_deref(),
        std::env::var("EDITOR").ok().as_deref(),
    )
}

/// The process that runs `template` on `path`: the first word is the program,
/// the rest its arguments, `{path}` replaced or the path appended. A path
/// appended keeps its bytes, so a file name that is not UTF-8 still opens.
pub fn command(template: &str, path: &Path) -> Result<std::process::Command, String> {
    let words = split(template)?;
    let Some((program, rest)) = words.split_first() else {
        return Err(format!("the editor command `{template}` is empty"));
    };
    if program.contains("{path}") {
        return Err(format!("the editor command `{template}` names no program"));
    }
    let mut command = std::process::Command::new(program);
    if rest.iter().any(|w| w.contains("{path}")) {
        let shown = path.to_string_lossy();
        command.args(rest.iter().map(|w| w.replace("{path}", &shown)));
    } else {
        command.args(rest).arg(OsString::from(path));
    }
    Ok(command)
}

/// Whether a template runs a terminal editor: its program is one of
/// [`TERMINAL_EDITORS`], or one of [`EMACS_PROGRAMS`] with a terminal flag.
pub fn needs_terminal(template: &str) -> bool {
    let Ok(words) = split(template) else {
        return false;
    };
    let Some((program, rest)) = words.split_first() else {
        return false;
    };
    let name = Path::new(program)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.clone());
    TERMINAL_EDITORS.contains(&name.as_str())
        || (EMACS_PROGRAMS.contains(&name.as_str())
            && rest
                .iter()
                .any(|word| EMACS_TERMINAL_FLAGS.contains(&word.as_str())))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The program and arguments [`command`] builds.
    fn argv(template: &str, path: &str) -> Vec<String> {
        let command = command(template, Path::new(path)).expect("splits");
        std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|w| w.to_string_lossy().into_owned())
            .collect()
    }

    /// The reviewer's case, pinned both ways: a `$VISUAL` naming a GUI
    /// editor gives way to a terminal `$EDITOR`, and alone it runs as its
    /// program and its flag, not as one program named `code -w`.
    #[test]
    fn a_gui_visual_gives_way_to_a_terminal_editor_and_alone_runs_with_its_flag() {
        assert_eq!(template_from(Some("code -w"), Some("nvim")), "nvim");
        assert_eq!(
            pick(Some("code -w"), Some("nvim")),
            Ok(Some((EnvVar::Editor, "nvim".to_string())))
        );
        assert_eq!(template_from(Some("code -w"), None), "code -w");
        assert_eq!(
            pick(Some("code -w"), Some("zed --wait")),
            Err((EnvVar::Visual, "code -w".to_string())),
            "two GUI editors: the first set one"
        );
        assert_eq!(
            argv("code -w", "/c/config.toml"),
            ["code", "-w", "/c/config.toml"]
        );
    }

    #[test]
    fn an_editor_with_a_quoted_argument_runs_as_its_words() {
        let editor = template_from(None, Some("nvim -u 'my init.lua'"));
        assert_eq!(
            argv(&editor, "/c/config.toml"),
            ["nvim", "-u", "my init.lua", "/c/config.toml"]
        );
        assert_eq!(
            argv("hx +3 {path}", "/c/a b.toml"),
            ["hx", "+3", "/c/a b.toml"]
        );
        assert!(command("  ", Path::new("/c/x")).is_err());
        assert!(command("'nvim", Path::new("/c/x")).is_err());
    }

    #[test]
    fn a_terminal_visual_wins_over_editor() {
        assert_eq!(template_from(Some("nvim"), Some("vi")), "nvim");
        assert_eq!(
            template_from(Some("/usr/bin/vim -u x"), Some("hx")),
            "/usr/bin/vim -u x"
        );
    }

    #[test]
    fn editor_is_taken_when_visual_is_unset_or_blank() {
        assert_eq!(template_from(None, Some("vi")), "vi");
        assert_eq!(template_from(Some("  "), Some("vi")), "vi");
    }

    #[test]
    fn the_fallback_runs_when_neither_names_one() {
        assert_eq!(template_from(None, None), FALLBACK);
        assert_eq!(template_from(Some(""), Some(" ")), FALLBACK);
        assert_eq!(pick(None, Some(" ")), Ok(None));
    }

    /// A one-word terminal editor the first list did not know ran the
    /// `$EDITOR` beside it instead (PERSO-320 review).
    #[test]
    fn the_common_terminal_editors_are_known() {
        for editor in [
            "lvim", "nvi", "elvis", "vile", "vis", "jed", "zile", "mcedit", "amp", "dte", "ee",
            "jmacs", "jpico", "jstar", "tilde", "ox",
        ] {
            assert!(needs_terminal(editor), "{editor}");
        }
        assert_eq!(template_from(Some("lvim"), Some("vi")), "lvim");
        assert_eq!(template_from(Some("vis"), Some("vi")), "vis");
        assert_eq!(template_from(Some("mcedit"), Some("vi")), "mcedit");
    }

    /// Emacs and emacsclient run in the terminal with `-nw`, `-t`, `--tty`
    /// or `--no-window-system`, and open a window without them.
    #[test]
    fn emacs_needs_a_terminal_only_with_its_terminal_flag() {
        for editor in [
            "emacs -nw",
            "emacs --no-window-system",
            "/usr/bin/emacs -q -nw",
            "emacsclient -t",
            "emacsclient -nw",
            "emacsclient --tty -a ''",
        ] {
            assert!(needs_terminal(editor), "{editor}");
        }
        for editor in ["emacs", "emacsclient -c", "emacs -q", "code -nw"] {
            assert!(!needs_terminal(editor), "{editor}");
        }
        assert_eq!(
            template_from(Some("emacsclient -t"), Some("vi")),
            "emacsclient -t"
        );
        assert_eq!(template_from(Some("emacs"), Some("vi")), "vi");
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
}
