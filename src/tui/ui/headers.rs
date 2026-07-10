use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::super::app::{App, Focus};
use super::super::theme;
use super::util::pane_border_style;

pub(super) fn header_line<'a>(label: &'a str, value: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!(" {label}: "),
            Style::default()
                .fg(theme::MAUVE)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(value, Style::default().fg(theme::TEXT)),
    ])
}

pub(super) fn render_headers(app: &App, frame: &mut Frame, area: Rect) {
    let border_style = pane_border_style(app.focus, Focus::Headers);
    let block = Block::default()
        .title(" Headers ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .style(Style::default().bg(theme::BASE));

    let selected = app.selected_email();
    if selected.is_none() {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new("  No email selected").style(Style::default().fg(theme::SUBTEXT0)),
            inner,
        );
        return;
    }

    let email = selected.unwrap();
    let mut lines: Vec<Line> = Vec::new();

    lines.push(header_line("From", &email.from));
    lines.push(header_line("To", &email.to));
    if let Some(cc) = &email.cc {
        if !cc.is_empty() {
            lines.push(header_line("Cc", cc));
        }
    }
    lines.push(header_line("Subj", &email.subject));

    let date_status = format!("{}  [{}]", email.date_display, email.status);
    lines.push(header_line("Date", &date_status));

    let content = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.headers_scroll, 0));
    frame.render_widget(content, area);
}
