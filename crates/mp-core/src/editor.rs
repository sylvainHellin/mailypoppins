//! The terminal editor the CLI and the TUI hand a file to: a draft (`mp edit`,
//! the TUI's `e`), a signature, the log, and `config.toml` (`mp config open`,
//! the TUI's `sc`, PERSO-320).
//!
//! One resolver for all of them, so a file opens in the editor the user named
//! whichever command opened it. The desktop app resolves its own
//! (`clients/desktop/src-tauri/src/editor.rs`), with a setting and a terminal
//! to lend, and reads `$VISUAL` before `$EDITOR` in the same order.

/// What runs when neither variable names an editor.
pub const FALLBACK: &str = "hx";

/// The editor command: `$VISUAL`, else `$EDITOR`, else [`FALLBACK`].
///
/// A variable that is set but blank counts as unset, as it does for the
/// desktop app: a `VISUAL=` left in a shell profile names no program, and
/// running the empty string would fail instead of falling through.
pub fn command() -> String {
    pick(
        std::env::var("VISUAL").ok().as_deref(),
        std::env::var("EDITOR").ok().as_deref(),
    )
}

/// [`command`] over the two values, for a test that sets no environment.
pub fn pick(visual: Option<&str>, editor: Option<&str>) -> String {
    [visual, editor]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|value| !value.is_empty())
        .unwrap_or(FALLBACK)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visual_wins_over_editor() {
        assert_eq!(pick(Some("nvim"), Some("vi")), "nvim");
    }

    #[test]
    fn editor_is_taken_when_visual_is_unset_or_blank() {
        assert_eq!(pick(None, Some("vi")), "vi");
        assert_eq!(pick(Some("  "), Some("vi")), "vi");
    }

    #[test]
    fn the_fallback_runs_when_neither_names_one() {
        assert_eq!(pick(None, None), FALLBACK);
        assert_eq!(pick(Some(""), Some(" ")), FALLBACK);
    }
}
