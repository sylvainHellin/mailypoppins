use ratatui::layout::{Alignment, Constraint, Direction, Flex, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use super::super::app::{ActivityLevelFilter, App, StatusLevel};
use super::super::theme;

pub(super) fn render_activity_overlay(app: &mut App, frame: &mut Frame, area: Rect) {
    let overlay_width = (area.width * 8 / 10)
        .max(40)
        .min(area.width.saturating_sub(4));
    let overlay_height = (area.height * 80 / 100)
        .max(12)
        .min(area.height.saturating_sub(2));

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(overlay_width)])
        .flex(Flex::Center)
        .split(area);

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(overlay_height)])
        .flex(Flex::Center)
        .split(horizontal[0]);

    let overlay_area = vertical[0];

    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default().style(Style::default().bg(theme::active().bg)),
        area,
    );

    // Build filtered entries: at or above the chosen level, then the text filter
    let filter = app.activity_filter.to_lowercase();
    let level = app.activity_level;
    let entries: Vec<_> = app
        .status_log
        .iter()
        .filter(|e| level.keeps(&e.level))
        .filter(|e| {
            filter.is_empty()
                || e.message.to_lowercase().contains(&filter)
                || format!("{:?}", e.level).to_lowercase().contains(&filter)
        })
        .collect();

    let total = entries.len();

    let bottom_title = if app.activity_filter_active {
        " Type to filter | Esc: clear ".to_string()
    } else if total == 0 {
        " /filter  l level  Esc: close ".to_string()
    } else {
        " j/k scroll  gg/G top/bottom  d/u page  /filter  l level  Esc: close ".to_string()
    };

    let border_color = theme::active().accent_alt;
    let block = Block::default()
        .title(format!(" Activity Log ({total}) "))
        .title(Line::from(format!(" Level: {} ", level.label())).alignment(Alignment::Right))
        .title_bottom(Line::from(bottom_title).alignment(Alignment::Center))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(theme::active().bg));

    let inner = block.inner(overlay_area);
    frame.render_widget(block, overlay_area);

    // Filter bar
    let show_filter_bar = app.activity_filter_active || !app.activity_filter.is_empty();
    let filter_bar_height: u16 = if show_filter_bar && inner.height >= 3 {
        1
    } else {
        0
    };

    let content_area = if filter_bar_height > 0 && inner.height >= 2 {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(inner);

        let mut spans = vec![
            Span::styled("/", Style::default().fg(theme::active().accent_alt)),
            Span::styled(
                app.activity_filter.as_str(),
                Style::default().fg(theme::active().text),
            ),
        ];
        if app.activity_filter_active {
            spans.push(Span::styled(
                "\u{2588}",
                Style::default().fg(theme::active().accent_alt),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), chunks[0]);
        chunks[1]
    } else {
        inner
    };

    if entries.is_empty() {
        let msg = if filter.is_empty() && level == ActivityLevelFilter::All {
            "  No activity yet"
        } else {
            "  No matching entries"
        };
        let empty = Paragraph::new(msg).style(Style::default().fg(theme::active().text_muted));
        frame.render_widget(empty, content_area);
        return;
    }

    // Build lines with full (unwrapped) messages -- one entry can span multiple lines
    let available_width = content_area.width.saturating_sub(1) as usize;
    let time_prefix_len = 8; // " HH:MM  "
    let msg_width = available_width.saturating_sub(time_prefix_len);

    let mut lines: Vec<Line> = Vec::new();

    for entry in &entries {
        let time = entry.timestamp.format("%H:%M").to_string();
        let color = level_color(&entry.level);

        if msg_width == 0 || entry.message.is_empty() {
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {time}  "),
                    Style::default().fg(theme::active().text_faint),
                ),
                Span::styled(&entry.message, Style::default().fg(color)),
            ]));
            continue;
        }

        // Word-wrap the message manually so each visual line is a Line
        let wrapped = word_wrap(&entry.message, msg_width);
        for (i, chunk) in wrapped.iter().enumerate() {
            if i == 0 {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!(" {time}  "),
                        Style::default().fg(theme::active().text_faint),
                    ),
                    Span::styled(chunk.to_string(), Style::default().fg(color)),
                ]));
            } else {
                // Continuation lines: indent to align with message start
                let pad = " ".repeat(time_prefix_len);
                lines.push(Line::from(vec![
                    Span::styled(pad, Style::default().fg(theme::active().text_faint)),
                    Span::styled(chunk.to_string(), Style::default().fg(color)),
                ]));
            }
        }
    }

    let content_height = lines.len() as u16;
    let visible_height = content_area.height;
    let max_scroll = content_height.saturating_sub(visible_height);
    app.activity_scroll = app.activity_scroll.min(max_scroll);

    let content = Paragraph::new(lines).scroll((app.activity_scroll, 0));
    frame.render_widget(content, content_area);
}

fn level_color(level: &StatusLevel) -> ratatui::style::Color {
    match level {
        StatusLevel::Success => theme::active().success,
        StatusLevel::Error => theme::active().error,
        StatusLevel::Warning => theme::active().warning,
        StatusLevel::Info => theme::active().info,
        StatusLevel::Progress => theme::active().accent_alt,
    }
}

/// Simple word-wrap: break `text` into lines of at most `width` characters.
/// Tries to break at word boundaries when possible.
fn word_wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }

    let mut result = Vec::new();
    let mut current = String::new();

    for word in text.split_whitespace() {
        if current.is_empty() {
            if word.len() > width {
                // Break long word across lines
                let mut remaining = word;
                while remaining.len() > width {
                    let boundary = floor_char_boundary(remaining, width);
                    result.push(remaining[..boundary].to_string());
                    remaining = &remaining[boundary..];
                }
                current = remaining.to_string();
            } else {
                current = word.to_string();
            }
        } else if current.len() + 1 + word.len() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            result.push(current);
            if word.len() > width {
                let mut remaining = word;
                while remaining.len() > width {
                    let boundary = floor_char_boundary(remaining, width);
                    result.push(remaining[..boundary].to_string());
                    remaining = &remaining[boundary..];
                }
                current = remaining.to_string();
            } else {
                current = word.to_string();
            }
        }
    }

    if !current.is_empty() {
        result.push(current);
    }

    if result.is_empty() {
        result.push(String::new());
    }

    result
}

/// Find the largest byte index <= `max` that is a char boundary in `s`.
///
/// When no char fits in `max` bytes (a 3-byte CJK char in a 2-byte slot),
/// the end of the first char is returned instead, so a caller slicing
/// `s[..boundary]` always gets a non-empty, valid prefix and makes progress.
fn floor_char_boundary(s: &str, max: usize) -> usize {
    if max >= s.len() {
        return s.len();
    }
    let mut i = max;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    if i == 0 {
        s.chars().next().map_or(0, char::len_utf8)
    } else {
        i
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn overlay_text(app: &mut App) -> String {
        let (w, h) = (100, 16);
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|frame| render_activity_overlay(app, frame, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// PERSO-218: `l` cycles the lowest level the overlay lists, all,
    /// warnings and errors, errors, and back; the title counts what is listed
    /// and its right corner names the level.
    #[test]
    fn l_lists_only_the_lines_at_or_above_the_chosen_level() {
        let mut app = App::default_for_tests();
        app.push_status("note-info".into(), StatusLevel::Info);
        app.push_status("note-success".into(), StatusLevel::Success);
        app.push_status("note-progress".into(), StatusLevel::Progress);
        app.push_status("note-warning".into(), StatusLevel::Warning);
        app.push_status("note-error".into(), StatusLevel::Error);
        app.handle_key(KeyEvent::from(KeyCode::Char('s')));
        app.handle_key(KeyEvent::from(KeyCode::Char('l')));
        assert!(matches!(app.overlay, crate::app::Overlay::Activity));

        let all = overlay_text(&mut app);
        assert!(all.contains("Activity Log (5)"), "{all}");
        assert!(all.contains("Level: all"), "{all}");
        assert!(all.contains("l level"), "{all}");

        app.handle_key(KeyEvent::from(KeyCode::Char('l')));
        assert_eq!(app.activity_level, ActivityLevelFilter::Warnings);
        let warnings = overlay_text(&mut app);
        assert!(warnings.contains("Activity Log (2)"), "{warnings}");
        assert!(
            warnings.contains("Level: warnings and errors"),
            "{warnings}"
        );
        assert!(warnings.contains("note-warning"), "{warnings}");
        assert!(warnings.contains("note-error"), "{warnings}");
        for below in ["note-info", "note-success", "note-progress"] {
            assert!(!warnings.contains(below), "{below} listed: {warnings}");
        }

        app.handle_key(KeyEvent::from(KeyCode::Char('l')));
        let errors = overlay_text(&mut app);
        assert!(errors.contains("Activity Log (1)"), "{errors}");
        assert!(errors.contains("Level: errors"), "{errors}");
        assert!(errors.contains("note-error"), "{errors}");
        assert!(!errors.contains("note-warning"), "{errors}");

        app.handle_key(KeyEvent::from(KeyCode::Char('l')));
        assert_eq!(app.activity_level, ActivityLevelFilter::All);
        assert!(overlay_text(&mut app).contains("Activity Log (5)"));
    }

    /// The level and the text filter combine, a level with no line says so,
    /// and the overlay opens again at all levels.
    #[test]
    fn the_level_combines_with_the_filter_and_resets_on_close() {
        let mut app = App::default_for_tests();
        app.push_status("alpha synced".into(), StatusLevel::Info);
        app.push_status("alpha refused".into(), StatusLevel::Warning);
        app.push_status("beta refused".into(), StatusLevel::Warning);
        for c in ['s', 'l', 'l', '/', 'a', 'l', 'p', 'h', 'a'] {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        let text = overlay_text(&mut app);
        assert!(text.contains("Activity Log (1)"), "{text}");
        assert!(text.contains("alpha refused"), "{text}");

        app.handle_key(KeyEvent::from(KeyCode::Char('l')));
        let text = overlay_text(&mut app);
        assert!(text.contains("Activity Log (0)"), "{text}");
        assert!(text.contains("No matching entries"), "{text}");

        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(app.activity_level, ActivityLevelFilter::All);
        for c in ['s', 'l'] {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        assert_eq!(app.activity_level, ActivityLevelFilter::All);
        assert!(overlay_text(&mut app).contains("Level: all"));
    }

    #[test]
    fn word_wrap_narrower_than_one_char_does_not_panic() {
        assert_eq!(
            word_wrap("\u{65e5}\u{672c}\u{8a9e}", 2),
            ["\u{65e5}", "\u{672c}", "\u{8a9e}"]
        );
        assert_eq!(
            word_wrap("ok \u{65e5}\u{672c}", 2),
            ["ok", "\u{65e5}", "\u{672c}"]
        );
        assert_eq!(floor_char_boundary("\u{65e5}", 0), 3);
    }
}
