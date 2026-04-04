use ratatui::style::Style;
use ratatui::text::Span;

use super::super::app::Focus;
use super::super::theme;

pub(super) fn pane_border_style(current_focus: Focus, pane: Focus) -> Style {
    let focused = current_focus == pane || (current_focus == Focus::Search && pane == Focus::List);
    if focused {
        Style::default().fg(theme::PEACH)
    } else {
        Style::default().fg(theme::BLUE)
    }
}

pub(super) fn hint_span(key: &str) -> Span<'_> {
    Span::styled(key, Style::default().fg(theme::BLUE))
}

pub(super) fn desc_span(desc: &str) -> Span<'_> {
    Span::styled(desc, Style::default().fg(theme::SUBTEXT0))
}

pub(super) fn truncate(s: &str, max_width: usize) -> String {
    if max_width <= 3 {
        return s.chars().take(max_width).collect();
    }
    let char_count = s.chars().count();
    if char_count <= max_width {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_width - 1).collect();
        format!("{truncated}\u{2026}")
    }
}
