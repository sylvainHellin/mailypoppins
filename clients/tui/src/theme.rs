//! Named TUI color themes (helix-style): built-in themes are compiled in and
//! selected by name via the top-level `theme = "..."` key in config.toml.
//!
//! Colors are grouped into semantic slots (`Theme` fields) rather than raw
//! palette names, so a theme only has to answer "what color is an unread
//! marker / a focused border / an error" and the UI code never hardcodes a
//! palette. The default theme (`mailypoppins`) carries the Mac app's dark
//! palette, so the TUI and the app look like one product; `catppuccin-mocha`
//! keeps the TUI's original hardcoded appearance.

use std::sync::OnceLock;

use ratatui::style::Color;

/// Name of the theme used when none is configured (or the configured name is
/// unknown): the Mac app's dark palette.
pub const DEFAULT_THEME_NAME: &str = "mailypoppins";

/// Canonical names of all built-in themes, for error messages and docs.
pub const THEME_NAMES: &[&str] = &[
    "mailypoppins",
    "mailypoppins-light",
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
    /// Background of modal/overlay panels, distinct from `bg` so a modal
    /// reads as raised above the dimmed main view (#0032); `Reset` for the
    /// terminal-adaptive theme, which has no tone of its own to spare.
    /// Also the tint of an expanded conversation's rows in the mail list
    /// (PERSO-179), the quieter of the two fills a list row can take.
    pub panel_bg: Color,

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
            panel_bg: Color::Rgb(24, 24, 37),          // mantle (below base)
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

    /// Catppuccin Latte (light variant), deliberately not the upstream palette
    /// in five slots: `text_muted` takes subtext1 instead of subtext0, and
    /// `selection`, `unread`, `accent` and `warning` are darkened versions of
    /// green, blue, blue and yellow. Upstream Latte puts those colours under
    /// the WCAG AA 4.5:1 text contrast on `bg` or `panel_bg`, and each slot
    /// draws text, so each is moved just far enough to reach it (PERSO-179).
    /// Re-syncing with upstream must keep these five, or the light theme's
    /// muted, selected, unread, accent and warning text falls under 4.5:1
    /// again.
    pub const fn catppuccin_latte() -> Self {
        Self {
            bg: Color::Rgb(239, 241, 245),            // base
            surface: Color::Rgb(204, 208, 218),       // surface0
            text: Color::Rgb(76, 79, 105),            // text
            text_muted: Color::Rgb(92, 95, 119),      // subtext1, subtext0 is under 4.5:1
            text_faint: Color::Rgb(156, 160, 176),    // overlay0
            border: Color::Rgb(30, 102, 245),         // blue
            border_focused: Color::Rgb(254, 100, 11), // peach
            panel_bg: Color::Rgb(230, 233, 239),      // mantle (below base)
            selection: Color::Rgb(48, 120, 32),       // green darkened to 4.5:1
            unread: Color::Rgb(13, 90, 244),          // blue darkened to 4.5:1
            unread_count: Color::Rgb(254, 100, 11),   // peach
            accent: Color::Rgb(13, 90, 244),          // blue darkened to 4.5:1
            accent_alt: Color::Rgb(23, 146, 153),     // teal
            heading: Color::Rgb(136, 57, 239),        // mauve
            emphasis: Color::Rgb(223, 142, 29),       // yellow
            code: Color::Rgb(254, 100, 11),           // peach
            success: Color::Rgb(64, 160, 43),         // green
            warning: Color::Rgb(146, 93, 19),         // yellow darkened to 4.5:1
            error: Color::Rgb(210, 15, 57),           // red
            info: Color::Rgb(30, 102, 245),           // blue
        }
    }

    /// The Mac app's dark palette (clients/desktop/src/index.css, Basenord
    /// Palette D inverted): Prussian ground, Cream text, Ocean for focus and
    /// links, Pumpkin for the cursor, and the app's terminal colours for the
    /// rest, so the TUI, the app and its `mailypoppins` Neovim colorscheme
    /// read as one product.
    pub const fn mailypoppins() -> Self {
        Self {
            bg: Color::Rgb(12, 27, 51),       // #0C1B33 Prussian, the app's canvas
            surface: Color::Rgb(23, 74, 104), // #174A68 the app's selection
            text: Color::Rgb(244, 241, 232),  // #F4F1E8 Cream
            text_muted: Color::Rgb(180, 191, 206), // #B4BFCE muted-foreground
            text_faint: Color::Rgb(125, 139, 163), // #7D8BA3 disabled-foreground
            border: Color::Rgb(125, 139, 163), // #7D8BA3
            border_focused: Color::Rgb(75, 163, 204), // #4BA3CC ring, Ocean lightened
            panel_bg: Color::Rgb(20, 42, 74), // #142A4A card, raised above bg
            selection: Color::Rgb(255, 138, 61), // #FF8A3D Pumpkin, readable as text
            unread: Color::Rgb(75, 163, 204), // #4BA3CC
            unread_count: Color::Rgb(255, 138, 61), // #FF8A3D
            accent: Color::Rgb(109, 182, 218), // #6DB6DA link
            accent_alt: Color::Rgb(92, 200, 200), // #5CC8C8 terminal cyan
            heading: Color::Rgb(215, 162, 232), // #D7A2E8 terminal magenta
            emphasis: Color::Rgb(242, 193, 78), // #F2C14E terminal yellow
            code: Color::Rgb(255, 138, 61),   // #FF8A3D
            success: Color::Rgb(143, 209, 158), // #8FD19E terminal green
            warning: Color::Rgb(255, 103, 0), // #FF6700 warning, Pumpkin
            error: Color::Rgb(255, 122, 133), // #FF7A85 destructive
            info: Color::Rgb(75, 163, 204),   // #4BA3CC
        }
    }

    /// The Mac app's light palette (Basenord Palette D): Cream ground,
    /// Prussian text, the same roles as [`Theme::mailypoppins`].
    pub const fn mailypoppins_light() -> Self {
        Self {
            bg: Color::Rgb(244, 241, 232),       // #F4F1E8 Cream, the app's canvas
            surface: Color::Rgb(207, 227, 238),  // #CFE3EE the app's selection
            text: Color::Rgb(12, 27, 51),        // #0C1B33 Prussian
            text_muted: Color::Rgb(70, 84, 108), // #46546C muted-foreground
            text_faint: Color::Rgb(119, 131, 154), // #77839A disabled-foreground
            border: Color::Rgb(119, 131, 154),   // #77839A
            border_focused: Color::Rgb(31, 111, 147), // #1F6F93 ring
            panel_bg: Color::Rgb(236, 232, 221), // #ECE8DD muted, set off from bg
            selection: Color::Rgb(178, 70, 0),   // #B24600 Pumpkin darkened, 4.5:1 on panel_bg
            unread: Color::Rgb(31, 111, 147),    // #1F6F93
            unread_count: Color::Rgb(178, 70, 0), // #B24600
            accent: Color::Rgb(26, 99, 133),     // #1A6385 link
            accent_alt: Color::Rgb(30, 122, 122), // #1E7A7A terminal cyan
            heading: Color::Rgb(138, 63, 160),   // #8A3FA0 terminal magenta
            emphasis: Color::Rgb(138, 97, 0),    // #8A6100 terminal yellow
            code: Color::Rgb(160, 64, 0),        // #A04000
            success: Color::Rgb(47, 122, 62),    // #2F7A3E terminal green
            warning: Color::Rgb(178, 70, 0),     // #B24600 warning, the app's #B34700 a step darker
            error: Color::Rgb(180, 35, 46),      // #B4232E destructive
            info: Color::Rgb(31, 111, 147),      // #1F6F93
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
            panel_bg: Color::Rgb(22, 22, 30),          // darker than bg
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
            bg: Color::Reset,         // terminal default background
            surface: Color::DarkGray, // must contrast with bg (cursor row)
            text: Color::Reset,       // terminal default foreground
            text_muted: Color::Gray,
            text_faint: Color::Gray, // stays legible on the DarkGray status bar
            border: Color::Blue,
            border_focused: Color::Cyan,
            // Modals inherit the terminal background; `panel_contrast_fg`
            // falls back to `surface` so their border/dim still reads.
            panel_bg: Color::Reset,
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

    /// Readable foreground for text painted on an accent-colored background
    /// (badges, action-button chips). When `panel_bg` is `Color::Reset` (the
    /// terminal-adaptive theme), fall back to `surface` — which is guaranteed
    /// to contrast with the terminal background — instead of `bg`, which would
    /// be invisible on a `Reset` terminal. (#0032, herdr `panel_contrast_fg`.)
    pub fn panel_contrast_fg(&self) -> Color {
        match self.panel_bg {
            Color::Reset => self.surface,
            color => color,
        }
    }

    /// Look up a built-in theme by name (case-insensitive, with a few
    /// aliases). Returns `None` for unknown names.
    pub fn by_name(name: &str) -> Option<Self> {
        match name.trim().to_lowercase().as_str() {
            // Empty = unset (e.g. GlobalConfig::default()) -> default theme.
            "" | "default" | "mailypoppins" | "mailypoppins-dark" => Some(Self::mailypoppins()),
            "catppuccin" | "catppuccin-mocha" => Some(Self::catppuccin_mocha()),
            "latte" | "catppuccin-latte" => Some(Self::catppuccin_latte()),
            "mailypoppins-light" => Some(Self::mailypoppins_light()),
            "tokyo-night" | "tokyonight" => Some(Self::tokyo_night()),
            "terminal" | "transparent" | "ansi" => Some(Self::terminal()),
            _ => None,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::mailypoppins()
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
            let _ = ACTIVE.set(Theme::default());
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
    ACTIVE.get_or_init(Theme::default)
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
    fn default_theme_is_the_app_palette() {
        let theme = Theme::by_name(DEFAULT_THEME_NAME).unwrap();
        assert_eq!(theme, Theme::default());
        assert_eq!(theme, Theme::mailypoppins());
    }

    #[test]
    fn catppuccin_mocha_keeps_the_original_palette() {
        // The TUI's original hardcoded Catppuccin Mocha: base bg, text, accent.
        let theme = Theme::catppuccin_mocha();
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
        assert_eq!(Theme::by_name("default"), Some(Theme::mailypoppins()));
        assert_eq!(Theme::by_name(""), Some(Theme::mailypoppins()));
    }

    #[test]
    fn mailypoppins_themes_resolve_and_follow_the_app() {
        assert_eq!(Theme::by_name("mailypoppins"), Some(Theme::mailypoppins()));
        assert_eq!(
            Theme::by_name("Mailypoppins-Dark"),
            Some(Theme::mailypoppins())
        );
        assert_eq!(
            Theme::by_name("mailypoppins-light"),
            Some(Theme::mailypoppins_light())
        );
        // The grounds are the app's canvases: Prussian dark, Cream light.
        assert_eq!(Theme::mailypoppins().bg, Color::Rgb(12, 27, 51));
        assert_eq!(Theme::mailypoppins_light().bg, Color::Rgb(244, 241, 232));
        assert_eq!(Theme::mailypoppins().text, Theme::mailypoppins_light().bg);
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
            t.bg,
            t.surface,
            t.text,
            t.text_muted,
            t.text_faint,
            t.border,
            t.border_focused,
            t.panel_bg,
            t.selection,
            t.unread,
            t.unread_count,
            t.accent,
            t.accent_alt,
            t.heading,
            t.emphasis,
            t.code,
            t.success,
            t.warning,
            t.error,
            t.info,
        ] {
            assert!(
                !matches!(slot, Color::Rgb(..) | Color::Indexed(_)),
                "terminal theme slot {slot:?} is not an ANSI named color"
            );
        }
    }

    /// The WCAG relative luminance of an sRGB colour.
    fn luminance(color: Color) -> Option<f64> {
        let Color::Rgb(r, g, b) = color else {
            return None;
        };
        let channel = |v: u8| {
            let v = f64::from(v) / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        Some(0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b))
    }

    /// PERSO-179: every colour the list paints text in reads at 4.5:1, the
    /// WCAG AA bar for text, on both grounds a row can sit on, `bg` and the
    /// `panel_bg` of an expanded conversation, in every theme with colours of
    /// its own; the terminal theme's ANSI colours are the terminal's to pick.
    #[test]
    fn text_slots_reach_4_5_to_1_on_bg_and_panel_bg_in_every_opaque_theme() {
        let mut failures = Vec::new();
        let mut checked = 0;
        for name in THEME_NAMES {
            let t = Theme::by_name(name).expect("an advertised theme");
            if luminance(t.bg).is_none() {
                continue;
            }
            for (ground_name, ground) in [("bg", t.bg), ("panel_bg", t.panel_bg)] {
                for (slot, fg) in [
                    ("text", t.text),
                    ("text_muted", t.text_muted),
                    ("selection", t.selection),
                    ("accent", t.accent),
                    ("unread", t.unread),
                    ("warning", t.warning),
                ] {
                    let (a, b) = (
                        luminance(fg).expect("an RGB slot"),
                        luminance(ground).expect("an RGB ground"),
                    );
                    let ratio = (a.max(b) + 0.05) / (a.min(b) + 0.05);
                    checked += 1;
                    if ratio < 4.5 {
                        failures.push(format!("{name}: {slot} on {ground_name} is {ratio:.2}:1"));
                    }
                }
            }
        }
        assert!(checked >= 5 * 12, "every opaque theme is checked");
        assert!(failures.is_empty(), "{}", failures.join("\n"));
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
