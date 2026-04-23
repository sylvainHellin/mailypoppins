use ratatui::layout::{Alignment, Constraint, Direction, Flex, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use super::super::app::{App, StatusLevel};
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
        Block::default().style(Style::default().bg(theme::BASE)),
        area,
    );

    // Build filtered entries
    let filter = app.activity_filter.to_lowercase();
    let entries: Vec<_> = app
        .status_log
        .iter()
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
        " /filter  Esc: close ".to_string()
    } else {
        " j/k scroll  gg/G top/bottom  d/u page  /filter  Esc: close ".to_string()
    };

    let border_color = theme::TEAL;
    let block = Block::default()
        .title(format!(" Activity Log ({total}) "))
        .title_bottom(Line::from(bottom_title).alignment(Alignment::Center))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(theme::BASE));

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
            Span::styled("/", Style::default().fg(theme::TEAL)),
            Span::styled(app.activity_filter.as_str(), Style::default().fg(theme::TEXT)),
        ];
        if app.activity_filter_active {
            spans.push(Span::styled("\u{2588}", Style::default().fg(theme::TEAL)));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), chunks[0]);
        chunks[1]
    } else {
        inner
    };

    if entries.is_empty() {
        let msg = if filter.is_empty() {
            "  No activity yet"
        } else {
            "  No matching entries"
        };
        let empty = Paragraph::new(msg).style(Style::default().fg(theme::SUBTEXT0));
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
                Span::styled(format!(" {time}  "), Style::default().fg(theme::OVERLAY0)),
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
                        Style::default().fg(theme::OVERLAY0),
                    ),
                    Span::styled(chunk.to_string(), Style::default().fg(color)),
                ]));
            } else {
                // Continuation lines: indent to align with message start
                let pad = " ".repeat(time_prefix_len);
                lines.push(Line::from(vec![
                    Span::styled(pad, Style::default().fg(theme::OVERLAY0)),
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
        StatusLevel::Success => theme::GREEN,
        StatusLevel::Error => theme::RED,
        StatusLevel::Warning => theme::YELLOW,
        StatusLevel::Info => theme::BLUE,
        StatusLevel::Progress => theme::TEAL,
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
fn floor_char_boundary(s: &str, max: usize) -> usize {
    if max >= s.len() {
        return s.len();
    }
    let mut i = max;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    if i == 0 { max.min(s.len()) } else { i }
}
