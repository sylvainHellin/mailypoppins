//! Named TUI color themes (helix-style): built-in themes are compiled in and
//! selected by name via the top-level `theme = "..."` key in config.toml.
//!
//! Colors are grouped into semantic slots (`Theme` fields) rather than raw
//! palette names, so a theme only has to answer "what color is an unread
//! marker / a focused border / an error" and the UI code never hardcodes a
//! palette. The default theme (`catppuccin-mocha`) reproduces the original
//! hardcoded Catppuccin Mocha appearance exactly.

use std::sync::OnceLock;

use ratatui::style::Color;

/// Name of the theme used when none is configured (or the configured name is
/// unknown). Preserves the original appearance of the TUI.
pub const DEFAULT_THEME_NAME: &str = "catppuccin-mocha";

/// Canonical names of all built-in themes, for error messages and docs.
pub const THEME_NAMES: &[&str] = &[
    "catppuccin-mocha",
    "catppuccin-latte",
    "tokyo-night",
    "terminal",
];

/// Semantic color slots used by the TUI renderers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    // Surfaces
    /// Pane / overlay background.
    pub bg: Color,
    /// Raised surface: cursor-row background, status-bar background,
    /// code-block background.
    pub surface: Color,

    // Text
    /// Primary text.
    pub text: Color,
    /// Secondary text: table headers, key-hint descriptions, empty-state
    /// messages, read emails.
    pub text_muted: Color,
    /// Faint text: timestamps, separators, disabled items, placeholders.
    pub text_faint: Color,

    // Chrome
    /// Unfocused pane borders.
    pub border: Color,
    /// Focused pane border (and the attachment-picker accent).
    pub border_focused: Color,

    // Selection / activity states
    /// Cursor row foreground, highlighted sidebar item, checked checkboxes.
    pub selection: Color,
    /// Unread marker dot in the email list.
    pub unread: Color,
    /// Unread count in the status bar.
    pub unread_count: Color,

    // Accents
    /// Primary accent: key hints, links, search prefixes, active
    /// mailbox/account, quote bars, list bullets, H2 headings.
    pub accent: Color,
    /// Secondary accent: overlay borders and input cursors (compose, server
    /// search, activity log), WATCHING indicator, progress status, H3+
    /// headings.
    pub accent_alt: Color,

    // Content
    /// Header field labels, H1 headings, help section titles, picker cursor.
    pub heading: Color,
    /// Attention highlight that is not a warning: focused compose field
    /// label, selection count in the status bar, emphasized command names.
    pub emphasis: Color,
    /// Inline code and code blocks in the body preview.
    pub code: Color,

    // Status levels
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub info: Color,
}

impl Theme {
    /// The original hardcoded palette (Catppuccin Mocha).
    pub const fn catppuccin_mocha() -> Self {
        Self {
            bg: Color::Rgb(30, 30, 46),                // base
            surface: Color::Rgb(49, 50, 68),           // surface0
            text: Color::Rgb(205, 214, 244),           // text
            text_muted: Color::Rgb(166, 173, 200),     // subtext0
            text_faint: Color::Rgb(108, 112, 134),     // overlay0
            border: Color::Rgb(137, 180, 250),         // blue
            border_focused: Color::Rgb(250, 179, 135), // peach
            selection: Color::Rgb(166, 227, 161),      // green
            unread: Color::Rgb(137, 180, 250),         // blue
            unread_count: Color::Rgb(250, 179, 135),   // peach
            accent: Color::Rgb(137, 180, 250),         // blue
            accent_alt: Color::Rgb(148, 226, 213),     // teal
            heading: Color::Rgb(203, 166, 247),        // mauve
            emphasis: Color::Rgb(249, 226, 175),       // yellow
            code: Color::Rgb(250, 179, 135),           // peach
            success: Color::Rgb(166, 227, 161),        // green
            warning: Color::Rgb(249, 226, 175),        // yellow
            error: Color::Rgb(243, 139, 168),          // red
            info: Color::Rgb(137, 180, 250),           // blue
        }
    }

    /// Catppuccin Latte (light variant).
    pub const fn catppuccin_latte() -> Self {
        Self {
            bg: Color::Rgb(239, 241, 245),            // base
            surface: Color::Rgb(204, 208, 218),       // surface0
            text: Color::Rgb(76, 79, 105),            // text
            text_muted: Color::Rgb(108, 111, 133),    // subtext0
            text_faint: Color::Rgb(156, 160, 176),    // overlay0
            border: Color::Rgb(30, 102, 245),         // blue
            border_focused: Color::Rgb(254, 100, 11), // peach
            selection: Color::Rgb(64, 160, 43),       // green
            unread: Color::Rgb(30, 102, 245),         // blue
            unread_count: Color::Rgb(254, 100, 11),   // peach
            accent: Color::Rgb(30, 102, 245),         // blue
            accent_alt: Color::Rgb(23, 146, 153),     // teal
            heading: Color::Rgb(136, 57, 239),        // mauve
            emphasis: Color::Rgb(223, 142, 29),       // yellow
            code: Color::Rgb(254, 100, 11),           // peach
            success: Color::Rgb(64, 160, 43),         // green
            warning: Color::Rgb(223, 142, 29),        // yellow
            error: Color::Rgb(210, 15, 57),           // red
            info: Color::Rgb(30, 102, 245),           // blue
        }
    }

    /// Tokyo Night.
    pub const fn tokyo_night() -> Self {
        Self {
            bg: Color::Rgb(26, 27, 38),                // bg
            surface: Color::Rgb(41, 46, 66),           // bg_highlight
            text: Color::Rgb(192, 202, 245),           // fg
            text_muted: Color::Rgb(169, 177, 214),     // fg_dark
            text_faint: Color::Rgb(86, 95, 137),       // comment
            border: Color::Rgb(122, 162, 247),         // blue
            border_focused: Color::Rgb(255, 158, 100), // orange
            selection: Color::Rgb(158, 206, 106),      // green
            unread: Color::Rgb(122, 162, 247),         // blue
            unread_count: Color::Rgb(255, 158, 100),   // orange
            accent: Color::Rgb(122, 162, 247),         // blue
            accent_alt: Color::Rgb(115, 218, 202),     // teal
            heading: Color::Rgb(187, 154, 247),        // purple
            emphasis: Color::Rgb(224, 175, 104),       // yellow
            code: Color::Rgb(255, 158, 100),           // orange
            success: Color::Rgb(158, 206, 106),        // green
            warning: Color::Rgb(224, 175, 104),        // yellow
            error: Color::Rgb(247, 118, 142),          // red
            info: Color::Rgb(122, 162, 247),           // blue
        }
    }

    /// Terminal-adaptive theme: surfaces use `Color::Reset` so the terminal's
    /// own background shows through, and the rest of the palette uses the 16
    /// ANSI named colors so the whole TUI follows the user's terminal theme
    /// (light or dark).
    ///
    /// Two deliberate exceptions to "Reset everywhere":
    /// - `surface` is `DarkGray` (not `Reset`): the cursor row, status bar and
    ///   code blocks paint text over `surface` and rely on it contrasting with
    ///   `bg`. With `surface == Reset` the cursor row would be invisible.
    /// - `selection` is `White` so the cursor-row foreground stays legible on
    ///   the `DarkGray` surface on both light and dark terminals.
    pub const fn terminal() -> Self {
        Self {
            bg: Color::Reset,             // terminal default background
            surface: Color::DarkGray,     // must contrast with bg (cursor row)
            text: Color::Reset,           // terminal default foreground
            text_muted: Color::Gray,
            text_faint: Color::Gray, // stays legible on the DarkGray status bar
            border: Color::Blue,
            border_focused: Color::Cyan,
            selection: Color::White, // cursor-row fg over DarkGray surface
            unread: Color::Blue,
            unread_count: Color::Yellow,
            accent: Color::Blue,
            accent_alt: Color::Cyan,
            heading: Color::Magenta,
            emphasis: Color::Yellow,
            code: Color::Yellow,
            success: Color::Green,
            warning: Color::Yellow,
            error: Color::Red,
            info: Color::Blue,
        }
    }

    /// Look up a built-in theme by name (case-insensitive, with a few
    /// aliases). Returns `None` for unknown names.
    pub fn by_name(name: &str) -> Option<Self> {
        match name.trim().to_lowercase().as_str() {
            // Empty = unset (e.g. GlobalConfig::default()) -> default theme.
            "" | "default" | "catppuccin" | "catppuccin-mocha" => Some(Self::catppuccin_mocha()),
            "latte" | "catppuccin-latte" => Some(Self::catppuccin_latte()),
            "tokyo-night" | "tokyonight" => Some(Self::tokyo_night()),
            "terminal" | "transparent" | "ansi" => Some(Self::terminal()),
            _ => None,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::catppuccin_mocha()
    }
}

static ACTIVE: OnceLock<Theme> = OnceLock::new();

/// Select the process-wide theme by name. Only the first call takes effect
/// (the TUI calls this once at startup, before the first frame). Returns a
/// warning message when the name is unknown; the default theme is used
/// instead of failing.
pub fn init(name: &str) -> Option<String> {
    match Theme::by_name(name) {
        Some(theme) => {
            let _ = ACTIVE.set(theme);
            None
        }
        None => {
            let _ = ACTIVE.set(Theme::catppuccin_mocha());
            Some(format!(
                "Unknown theme '{}' in config.toml; using '{}'. Available: {}",
                name,
                DEFAULT_THEME_NAME,
                THEME_NAMES.join(", ")
            ))
        }
    }
}

/// The active theme. Falls back to the default theme when `init` was never
/// called (e.g. in tests).
pub fn active() -> &'static Theme {
    ACTIVE.get_or_init(Theme::catppuccin_mocha)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_advertised_names_resolve() {
        for name in THEME_NAMES {
            assert!(Theme::by_name(name).is_some(), "theme '{name}' missing");
        }
    }

    #[test]
    fn default_theme_preserves_original_palette() {
        // The default must keep today's exact appearance: base bg and text of
        // the original hardcoded Catppuccin Mocha palette.
        let theme = Theme::by_name(DEFAULT_THEME_NAME).unwrap();
        assert_eq!(theme, Theme::default());
        assert_eq!(theme.bg, Color::Rgb(30, 30, 46));
        assert_eq!(theme.text, Color::Rgb(205, 214, 244));
        assert_eq!(theme.accent, Color::Rgb(137, 180, 250));
    }

    #[test]
    fn lookup_is_case_insensitive_and_aliased() {
        assert_eq!(Theme::by_name("Tokyo-Night"), Some(Theme::tokyo_night()));
        assert_eq!(
            Theme::by_name("catppuccin"),
            Some(Theme::catppuccin_mocha())
        );
        assert_eq!(Theme::by_name("default"), Some(Theme::catppuccin_mocha()));
        assert_eq!(Theme::by_name(""), Some(Theme::catppuccin_mocha()));
    }

    #[test]
    fn terminal_theme_resolves_and_aliases() {
        assert_eq!(Theme::by_name("terminal"), Some(Theme::terminal()));
        assert_eq!(Theme::by_name("transparent"), Some(Theme::terminal()));
        assert_eq!(Theme::by_name("ansi"), Some(Theme::terminal()));
        assert_eq!(Theme::by_name("TERMINAL"), Some(Theme::terminal()));
    }

    #[test]
    fn terminal_theme_uses_reset_and_ansi_slots() {
        let t = Theme::terminal();
        // Surfaces let the terminal background show through...
        assert_eq!(t.bg, Color::Reset);
        assert_eq!(t.text, Color::Reset);
        // ...except `surface`, which must contrast with `bg` so the cursor
        // row / status bar stay visible on both light and dark terminals.
        assert_ne!(t.surface, Color::Reset);
        assert_eq!(t.surface, Color::DarkGray);
        // Status levels map to the expected ANSI named colors.
        assert_eq!(t.error, Color::Red);
        assert_eq!(t.warning, Color::Yellow);
        assert_eq!(t.success, Color::Green);
        // No RGB anywhere: the whole palette follows the terminal.
        for slot in [
            t.bg, t.surface, t.text, t.text_muted, t.text_faint, t.border,
            t.border_focused, t.selection, t.unread, t.unread_count, t.accent,
            t.accent_alt, t.heading, t.emphasis, t.code, t.success, t.warning,
            t.error, t.info,
        ] {
            assert!(
                !matches!(slot, Color::Rgb(..) | Color::Indexed(_)),
                "terminal theme slot {slot:?} is not an ANSI named color"
            );
        }
    }

    #[test]
    fn unknown_name_returns_none_and_init_warns() {
        assert!(Theme::by_name("solarized-nope").is_none());
        let warning = init("solarized-nope");
        // `init` may have been won by another test already (OnceLock), but
        // by_name-based fallback messaging is deterministic.
        if let Some(msg) = warning {
            assert!(msg.contains("solarized-nope"));
            assert!(msg.contains(DEFAULT_THEME_NAME));
        }
        // Never crashes; active() always yields a theme.
        let _ = active();
    }
}
