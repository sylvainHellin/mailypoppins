use ratatui::layout::{Alignment, Constraint, Direction, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use super::super::app::{App, AttachmentPicker, ConfirmDialog, PersistentError};
use super::super::theme;
use super::util::truncate;

pub(super) fn render_confirm_dialog(dialog: &ConfirmDialog, frame: &mut Frame, area: Rect) {
    let dialog_width = 40u16.min(area.width.saturating_sub(4));
    let dialog_height = 7u16;

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(dialog_width)])
        .flex(Flex::Center)
        .split(area);

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(dialog_height)])
        .flex(Flex::Center)
        .split(horizontal[0]);

    let dialog_area = vertical[0];

    frame.render_widget(Clear, dialog_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::YELLOW))
        .style(Style::default().bg(theme::BASE));

    let lines = vec![
        Line::from(Span::styled(
            &dialog.title,
            Style::default()
                .fg(theme::YELLOW)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            truncate(&dialog.detail, dialog_width.saturating_sub(4) as usize),
            Style::default().fg(theme::TEXT),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  [y]", Style::default().fg(theme::GREEN)),
            Span::styled("es  ", Style::default().fg(theme::TEXT)),
            Span::styled("[n]", Style::default().fg(theme::RED)),
            Span::styled("o", Style::default().fg(theme::TEXT)),
        ]),
    ];

    let content = Paragraph::new(lines).block(block);
    frame.render_widget(content, dialog_area);
}

pub(super) fn render_attachment_picker(picker: &AttachmentPicker, frame: &mut Frame, area: Rect) {
    let file_count = picker.files.len() as u16;
    let dialog_width = 50u16.min(area.width.saturating_sub(4));
    let dialog_height = (file_count + 4).min(area.height.saturating_sub(2));

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(dialog_width)])
        .flex(Flex::Center)
        .split(area);

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(dialog_height)])
        .flex(Flex::Center)
        .split(horizontal[0]);

    let dialog_area = vertical[0];
    frame.render_widget(Clear, dialog_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::PEACH))
        .style(Style::default().bg(theme::BASE));

    let inner_width = dialog_width.saturating_sub(4) as usize;
    let mut lines = vec![
        Line::from(Span::styled(
            "Select Attachment",
            Style::default().fg(theme::PEACH).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    for (i, path) in picker.files.iter().enumerate() {
        let name = path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        let display = if name.len() > inner_width {
            format!("{}...", &name[..inner_width.saturating_sub(3)])
        } else {
            name
        };
        let style = if i == picker.selected {
            Style::default().fg(theme::MAUVE).bg(theme::SURFACE0)
        } else {
            Style::default().fg(theme::TEXT)
        };
        lines.push(Line::from(Span::styled(display, style)));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "j/k nav  Enter open  Esc cancel",
        Style::default().fg(theme::SUBTEXT0),
    )));

    let content = Paragraph::new(lines).block(block);
    frame.render_widget(content, dialog_area);
}

pub(super) fn render_persistent_error(error: &PersistentError, frame: &mut Frame, area: Rect) {
    let dialog_width = 50u16.min(area.width.saturating_sub(4));
    let msg_lines = error.message.lines().count() as u16;
    let dialog_height = msg_lines + 6;
    let inner_width = dialog_width.saturating_sub(4) as usize;

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(dialog_width)])
        .flex(Flex::Center)
        .split(area);

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(dialog_height)])
        .flex(Flex::Center)
        .split(horizontal[0]);

    let dialog_area = vertical[0];
    frame.render_widget(Clear, dialog_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::RED))
        .style(Style::default().bg(theme::BASE));

    let mut lines = vec![
        Line::from(Span::styled(
            "Error",
            Style::default()
                .fg(theme::RED)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    for msg_line in error.message.lines() {
        lines.push(Line::from(Span::styled(
            truncate(msg_line, inner_width),
            Style::default().fg(theme::TEXT),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  [s]", Style::default().fg(theme::GREEN)),
        Span::styled("ync now  ", Style::default().fg(theme::TEXT)),
        Span::styled("[d]", Style::default().fg(theme::SUBTEXT0)),
        Span::styled("ismiss", Style::default().fg(theme::TEXT)),
    ]));

    let content = Paragraph::new(lines).block(block);
    frame.render_widget(content, dialog_area);
}

pub(super) fn help_sections() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    vec![
        ("GLOBAL", vec![
            ("q", "Quit"),
            ("`", "Switch account"),
            ("Ctrl+1-9", "Jump to account"),
            ("1-9", "Jump to mailbox"),
            ("s", "Focus sidebar"),
            ("Tab", "Cycle focus forward"),
            ("Shift+Tab", "Cycle focus backward"),
            ("/", "Filter by metadata"),
            ("\\", "Search email content"),
            ("?", "Toggle this help"),
            ("!", "Toggle activity log"),
        ]),
        ("SIDEBAR", vec![
            ("j/k", "Navigate mailboxes"),
            ("Enter/l", "Select mailbox"),
            ("Esc/h", "Return to list"),
        ]),
        ("EMAIL LIST", vec![
            ("j/k", "Navigate emails"),
            ("gg / G", "Jump to top / bottom"),
            ("Space", "Toggle selection"),
            ("Ctrl+a", "Select all visible"),
            ("Esc", "Clear selection"),
            ("h / l", "Focus sidebar / body"),
            ("Enter / e", "Open in editor"),
            ("r / R", "Reply / Reply-all"),
            ("w", "Forward"),
            ("a", "Archive"),
            ("d", "Delete"),
            ("A", "Approve draft"),
            ("x / X", "Send / Send all approved"),
            ("y", "Copy file path"),
            ("o", "Open attachment"),
            ("b", "Open HTML in browser"),
            ("n", "New draft"),
            ("f / F", "Fetch / Sync"),
            ("S", "Server search (IMAP)"),
        ]),
        ("HEADERS", vec![
            ("j/k", "Scroll headers"),
            ("h / l", "Back to list / body"),
        ]),
        ("BODY", vec![
            ("j/k", "Scroll line by line"),
            ("d/u", "Half-page down / up"),
            ("Esc/h", "Return to list"),
        ]),
    ]
}

pub(super) fn render_help_overlay(app: &mut App, frame: &mut Frame, area: Rect) {
    let help_width = 50u16.min(area.width.saturating_sub(4));
    let help_height = 42u16.min(area.height.saturating_sub(2));

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(help_width)])
        .flex(Flex::Center)
        .split(area);

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(help_height)])
        .flex(Flex::Center)
        .split(horizontal[0]);

    let help_area = vertical[0];
    frame.render_widget(Clear, help_area);

    let section_line = |title: &str| -> Line {
        Line::from(Span::styled(
            format!("  {title}"),
            Style::default()
                .fg(theme::MAUVE)
                .add_modifier(Modifier::BOLD),
        ))
    };

    let entry_line = |key: &str, desc: &str| -> Line {
        Line::from(vec![
            Span::styled(format!("  {key:<12}"), Style::default().fg(theme::BLUE)),
            Span::styled(desc.to_string(), Style::default().fg(theme::TEXT)),
        ])
    };

    let filter = app.help_filter.to_lowercase();
    let sections = help_sections();
    let mut lines: Vec<Line> = Vec::new();

    for (title, entries) in &sections {
        let matched: Vec<_> = if filter.is_empty() {
            entries.iter().collect()
        } else {
            entries.iter().filter(|(key, desc)| {
                key.to_lowercase().contains(&filter) || desc.to_lowercase().contains(&filter)
            }).collect()
        };

        if matched.is_empty() {
            continue;
        }

        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(section_line(title));
        for (key, desc) in matched {
            lines.push(entry_line(key, desc));
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No matching bindings",
            Style::default().fg(theme::OVERLAY0),
        )));
    }

    let content_height = lines.len() as u16;

    let show_filter_bar = app.help_filter_active || !app.help_filter.is_empty();
    let inner_height = help_area.height.saturating_sub(2);
    let filter_bar_height: u16 = if show_filter_bar && inner_height >= 3 { 1 } else { 0 };
    let visible_height = inner_height.saturating_sub(filter_bar_height);

    let max_scroll = content_height.saturating_sub(visible_height);
    app.help_scroll = app.help_scroll.min(max_scroll);

    let bottom_title = if content_height > visible_height {
        let top = app.help_scroll + 1;
        let bottom = (app.help_scroll + visible_height).min(content_height);
        format!(" {top}-{bottom}/{content_height} ")
    } else {
        " /filter  j/k scroll ".to_string()
    };

    let block = Block::default()
        .title(" Help ")
        .title_bottom(Line::from(bottom_title).alignment(Alignment::Center))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::BLUE))
        .style(Style::default().bg(theme::BASE));

    let block_inner = block.inner(help_area);
    frame.render_widget(block, help_area);

    let content_area = if filter_bar_height > 0 && block_inner.height >= 2 {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(block_inner);

        let mut spans = vec![
            Span::styled("/", Style::default().fg(theme::BLUE)),
            Span::styled(app.help_filter.as_str(), Style::default().fg(theme::TEXT)),
        ];
        if app.help_filter_active {
            spans.push(Span::styled("\u{2588}", Style::default().fg(theme::BLUE)));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), chunks[0]);

        chunks[1]
    } else {
        block_inner
    };

    let help = Paragraph::new(lines)
        .scroll((app.help_scroll, 0));
    frame.render_widget(help, content_area);
}
