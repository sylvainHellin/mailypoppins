use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

use super::super::app::{App, Focus, StatusLevel};
use super::super::theme;
use super::util::{pane_border_style, truncate};

pub(super) fn render_sidebar(app: &App, frame: &mut Frame, area: Rect) {
    let border_style = pane_border_style(app.focus, Focus::Sidebar);
    let sidebar_title = if app.accounts.len() > 1 {
        format!(" {} ", app.account_config.name.to_uppercase())
    } else {
        " Mail ".to_string()
    };
    let block = Block::default()
        .title(sidebar_title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .style(Style::default().bg(theme::active().bg));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();

    for (i, mb) in app.mailboxes.iter().enumerate() {
        let is_selected = i == app.active_mailbox;
        let is_highlighted = app.focus == Focus::Sidebar && i == app.sidebar_index;
        let count = app.mailbox_counts.get(i).copied().unwrap_or(0);

        let marker = if is_selected { ">" } else { " " };

        let label = format!("{} {} {} {:>2}", marker, mb.icon, mb.label, count);

        let style = if is_highlighted {
            Style::default()
                .fg(theme::active().selection)
                .add_modifier(Modifier::BOLD)
        } else if is_selected {
            Style::default().fg(theme::active().accent)
        } else {
            Style::default().fg(theme::active().text)
        };

        lines.push(Line::from(Span::styled(label, style)));
    }

    let sidebar_content = Paragraph::new(lines);
    frame.render_widget(sidebar_content, inner);
}

pub(super) fn render_activity_log(app: &App, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(" Activity ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::active().accent_alt))
        .style(Style::default().bg(theme::active().bg));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.status_log.is_empty() {
        let empty = Paragraph::new("  No activity yet")
            .style(Style::default().fg(theme::active().text_muted));
        frame.render_widget(empty, inner);
        return;
    }

    let visible = inner.height as usize;
    let skip = app.status_log.len().saturating_sub(visible);

    let lines: Vec<Line> = app
        .status_log
        .iter()
        .skip(skip)
        .take(visible)
        .map(|entry| {
            let time = entry.timestamp.format("%H:%M").to_string();
            let color = match entry.level {
                StatusLevel::Success => theme::active().success,
                StatusLevel::Error => theme::active().error,
                StatusLevel::Warning => theme::active().warning,
                StatusLevel::Info => theme::active().info,
                StatusLevel::Progress => theme::active().accent_alt,
            };
            Line::from(vec![
                Span::styled(
                    format!(" {time} "),
                    Style::default().fg(theme::active().text_faint),
                ),
                Span::styled(
                    truncate(&entry.message, inner.width.saturating_sub(7) as usize),
                    Style::default().fg(color),
                ),
            ])
        })
        .collect();

    let content = Paragraph::new(lines);
    frame.render_widget(content, inner);
}
