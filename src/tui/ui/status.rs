use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::super::app::App;
use super::super::theme;
use super::util::{desc_span, hint_span};

pub(super) fn render_status_bar(app: &App, frame: &mut Frame, area: Rect) {
    let total = app.mailbox_counts[app.active_mailbox];
    let shown = app.emails.len();
    let unread_count = app.emails.iter().filter(|e| !e.read).count();
    let any_watching = app.accounts.iter().any(|a| a.watcher_active);
    let watch_prefix = if any_watching { "WATCHING " } else { "" };
    let sel_text = if app.selection.is_empty() {
        String::new()
    } else {
        format!("{} sel | ", app.selection.len())
    };
    let unread_text = if unread_count > 0 {
        format!("{} unread | ", unread_count)
    } else {
        String::new()
    };
    let mailbox_text = if shown == 0 {
        format!("{} 0 ", app.active_label())
    } else if !app.search_query.is_empty() && shown != total {
        format!(
            "{} {}/{} ({}) ",
            app.active_label(),
            app.list_index + 1,
            shown,
            total
        )
    } else {
        format!("{} {}/{} ", app.active_label(), app.list_index + 1, shown)
    };
    let right_len =
        (sel_text.len() + unread_text.len() + watch_prefix.len() + mailbox_text.len() + 1) as u16;

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(right_len)])
        .split(area);

    let left_content = if app.bg_count > 0 {
        let frames = [
            '\u{280b}', '\u{2819}', '\u{2838}', '\u{2834}', '\u{2826}', '\u{2807}',
        ];
        let spinner = frames[app.bg_spin_tick % frames.len()];
        let label = app.status_message.as_deref().unwrap_or("Working...");
        let text = if app.bg_count > 1 {
            format!(" {} {} ({} ops)", spinner, label, app.bg_count)
        } else {
            format!(" {} {}", spinner, label)
        };
        Line::from(Span::styled(text, Style::default().fg(theme::GREEN)))
    } else if let Some(msg) = &app.status_message {
        Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(msg.as_str(), Style::default().fg(theme::GREEN)),
        ])
    } else {
        let mut spans: Vec<Span> = vec![Span::styled(" ", Style::default())];
        if app.accounts.len() > 1 {
            for (i, acct) in app.accounts.iter().enumerate() {
                let name = &acct.account_config.name;
                let label = name.to_uppercase();
                if i == app.active_account {
                    spans.push(Span::styled(
                        format!("[{}]", label),
                        Style::default().fg(theme::BASE).bg(theme::BLUE),
                    ));
                } else {
                    let style = if acct.has_unseen {
                        Style::default().fg(theme::GREEN)
                    } else {
                        Style::default().fg(theme::OVERLAY0)
                    };
                    spans.push(Span::styled(format!(" {} ", label), style));
                }
            }
            spans.push(Span::styled(" | ", Style::default().fg(theme::OVERLAY0)));
        }
        spans.push(hint_span("?"));
        spans.push(desc_span(" help"));
        Line::from(spans)
    };

    let left = Paragraph::new(left_content)
        .style(Style::default().fg(theme::SUBTEXT0).bg(theme::SURFACE0));
    frame.render_widget(left, chunks[0]);

    let mut right_spans = vec![Span::styled(" ", Style::default())];
    if !sel_text.is_empty() {
        right_spans.push(Span::styled(sel_text, Style::default().fg(theme::YELLOW)));
    }
    if !unread_text.is_empty() {
        right_spans.push(Span::styled(unread_text, Style::default().fg(theme::PEACH)));
    }
    if any_watching {
        right_spans.push(Span::styled(watch_prefix, Style::default().fg(theme::TEAL)));
    }
    right_spans.push(Span::styled(mailbox_text, Style::default().fg(theme::BLUE)));
    let right = Paragraph::new(Line::from(right_spans))
        .style(Style::default().bg(theme::SURFACE0))
        .alignment(Alignment::Right);
    frame.render_widget(right, chunks[1]);
}
