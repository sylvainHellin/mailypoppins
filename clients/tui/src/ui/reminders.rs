//! The reminders overlay (`R`, PERSO-255): one row per waiting reminder,
//! its summary, when it starts and where, and the account it belongs to.

use chrono::Utc;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::super::app::{App, RemindersOverlay, SNOOZE_MINUTES};
use super::super::theme;
use super::util::truncate;
use super::widgets::{modal_stack_areas, render_modal_shell};

pub(super) fn render_reminders_overlay(
    app: &App,
    overlay: &RemindersOverlay,
    frame: &mut Frame,
    area: Rect,
) {
    let height = (app.reminders.len() as u16).saturating_mul(2) + 5;
    let Some((_, inner)) = render_modal_shell(
        frame,
        area,
        72,
        height,
        theme::active().warning,
        theme::active().panel_bg,
    ) else {
        return;
    };
    let stack = modal_stack_areas(inner, 2, 1);
    frame.render_widget(
        Paragraph::new(Span::styled(
            "\u{25f7} Reminders",
            Style::default()
                .fg(theme::active().warning)
                .add_modifier(Modifier::BOLD),
        )),
        stack.header,
    );
    let width = stack.content.width as usize;
    let now = Utc::now();
    let zone = app.calendar_view.zone;
    let mut lines: Vec<Line> = Vec::new();
    for (i, reminder) in app.reminders.iter().enumerate() {
        let selected = i == overlay.selected;
        let marker = if selected { "\u{25b6} " } else { "  " };
        let text = format!(
            "{marker}{}",
            mp_client::format::reminder_line(reminder, now, &zone)
        );
        let style = if selected {
            Style::default()
                .fg(theme::active().selection)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::active().text)
        };
        lines.push(Line::from(Span::styled(truncate(&text, width), style)));
        let mut detail = format!("    {}", reminder.account);
        if let Some(url) = reminder.online_meeting_url.as_deref() {
            detail.push_str(&format!("  {url}"));
        }
        lines.push(Line::from(Span::styled(
            truncate(&detail, width),
            Style::default().fg(theme::active().text_muted),
        )));
    }
    frame.render_widget(Paragraph::new(lines), stack.content);
    if let Some(footer) = stack.footer {
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!(
                    "  s snooze {SNOOZE_MINUTES} min \u{b7} d dismiss \u{b7} Enter open \u{b7} Esc close"
                ),
                Style::default().fg(theme::active().text_muted),
            )),
            footer,
        );
    }
}
