use ratatui::layout::{Alignment, Constraint, Direction, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use super::super::app::{
    App, AttachmentPicker, AttachmentPickerMode, ConfirmDialog, DirPicker, DirPickerMode,
    PersistentError,
};
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

    let is_save = picker.mode == AttachmentPickerMode::Save;

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::PEACH))
        .style(Style::default().bg(theme::BASE));

    let inner_width = dialog_width.saturating_sub(4) as usize;
    let title = if is_save {
        "Save Attachments"
    } else {
        "Select Attachment"
    };
    let mut lines = vec![
        Line::from(Span::styled(
            title,
            Style::default()
                .fg(theme::PEACH)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    // Reserve space for checkbox prefix in save mode
    let name_width = if is_save {
        inner_width.saturating_sub(4)
    } else {
        inner_width
    };

    for (i, path) in picker.files.iter().enumerate() {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        let display = if name.len() > name_width {
            format!("{}...", &name[..name_width.saturating_sub(3)])
        } else {
            name
        };
        let cursor_style = if i == picker.selected {
            Style::default().fg(theme::MAUVE).bg(theme::SURFACE0)
        } else {
            Style::default().fg(theme::TEXT)
        };

        if is_save {
            let check = if picker.selected_set.contains(&i) {
                "\u{f0132} " // nf-md-checkbox_marked
            } else {
                "\u{f0131} " // nf-md-checkbox_blank_outline
            };
            let check_style = if picker.selected_set.contains(&i) {
                Style::default().fg(theme::GREEN)
            } else {
                Style::default().fg(theme::OVERLAY0)
            };
            lines.push(Line::from(vec![
                Span::styled(check, check_style),
                Span::styled(display, cursor_style),
            ]));
        } else {
            lines.push(Line::from(Span::styled(display, cursor_style)));
        }
    }

    lines.push(Line::from(""));
    let footer = if is_save {
        "j/k nav  Space sel  ^a all  Enter confirm  Esc cancel"
    } else {
        "j/k nav  Enter open  Esc cancel"
    };
    lines.push(Line::from(Span::styled(
        footer,
        Style::default().fg(theme::SUBTEXT0),
    )));

    let content = Paragraph::new(lines).block(block);
    frame.render_widget(content, dialog_area);
}

pub(super) fn render_dir_picker(picker: &DirPicker, frame: &mut Frame, area: Rect) {
    let dialog_width = 60u16.min(area.width.saturating_sub(4));
    let dialog_height = 20u16.min(area.height.saturating_sub(2));

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

    let mode_label = match picker.mode {
        DirPickerMode::Zoxide => "zoxide",
        DirPickerMode::Browser => "browse",
    };
    let title = format!(" Save to ({mode_label}) ");

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::GREEN))
        .style(Style::default().bg(theme::BASE));

    let block_inner = block.inner(dialog_area);
    frame.render_widget(block, dialog_area);

    let inner_width = block_inner.width.saturating_sub(2) as usize;

    match picker.mode {
        DirPickerMode::Zoxide => {
            // Input line + results + footer
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1), // query input
                    Constraint::Min(0),    // results
                    Constraint::Length(1), // footer
                ])
                .split(block_inner);

            // Query input
            let input_spans = vec![
                Span::styled("> ", Style::default().fg(theme::GREEN)),
                Span::styled(&picker.query, Style::default().fg(theme::TEXT)),
                Span::styled("\u{2588}", Style::default().fg(theme::GREEN)),
            ];
            frame.render_widget(Paragraph::new(Line::from(input_spans)), chunks[0]);

            // Results
            let mut result_lines: Vec<Line> = Vec::new();
            if picker.zoxide_results.is_empty() {
                if !picker.zoxide_available {
                    result_lines.push(Line::from(Span::styled(
                        "zoxide not found -- press Tab to browse",
                        Style::default().fg(theme::OVERLAY0),
                    )));
                } else if picker.query.is_empty() {
                    result_lines.push(Line::from(Span::styled(
                        "Type to search directories...",
                        Style::default().fg(theme::OVERLAY0),
                    )));
                } else {
                    result_lines.push(Line::from(Span::styled(
                        "No matches",
                        Style::default().fg(theme::OVERLAY0),
                    )));
                }
            } else {
                for (i, path) in picker.zoxide_results.iter().enumerate() {
                    let display = path.display().to_string();
                    let display = truncate(&display, inner_width);
                    let style = if i == picker.selected {
                        Style::default().fg(theme::MAUVE).bg(theme::SURFACE0)
                    } else {
                        Style::default().fg(theme::TEXT)
                    };
                    result_lines.push(Line::from(Span::styled(display, style)));
                }
            }
            frame.render_widget(Paragraph::new(result_lines), chunks[1]);

            // Footer
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "Tab browse  Enter save  Esc cancel",
                    Style::default().fg(theme::SUBTEXT0),
                ))),
                chunks[2],
            );
        }
        DirPickerMode::Browser => {
            // Header (current path) + entries + footer
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1), // current path
                    Constraint::Min(0),    // entries
                    Constraint::Length(1), // footer
                ])
                .split(block_inner);

            // Current path
            let path_str = picker.current_dir.display().to_string();
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    truncate(&path_str, inner_width),
                    Style::default()
                        .fg(theme::BLUE)
                        .add_modifier(Modifier::BOLD),
                ))),
                chunks[0],
            );

            // Entries: index 0 = "[ Save here ]", then directories
            let mut entry_lines: Vec<Line> = Vec::new();
            let save_here_style = if picker.selected == 0 {
                Style::default()
                    .fg(theme::GREEN)
                    .bg(theme::SURFACE0)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::GREEN)
            };
            entry_lines.push(Line::from(Span::styled("[ Save here ]", save_here_style)));

            for (i, dir) in picker.dir_entries.iter().enumerate() {
                let name = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| dir.display().to_string());
                let display = format!("\u{f024b} {}", truncate(&name, inner_width.saturating_sub(2)));
                let style = if i + 1 == picker.selected {
                    Style::default().fg(theme::MAUVE).bg(theme::SURFACE0)
                } else {
                    Style::default().fg(theme::TEXT)
                };
                entry_lines.push(Line::from(Span::styled(display, style)));
            }

            // Scroll the list if it's too tall
            let visible_height = chunks[1].height as usize;
            let scroll_offset = if picker.selected >= visible_height {
                picker.selected.saturating_sub(visible_height / 2)
            } else {
                0
            };

            frame.render_widget(
                Paragraph::new(entry_lines).scroll((scroll_offset as u16, 0)),
                chunks[1],
            );

            // Footer
            let footer_text = if picker.zoxide_available {
                "Tab zoxide  Enter open/save  h up  ~ home  Esc cancel"
            } else {
                "Enter open/save  h up  ~ home  Esc cancel"
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    footer_text,
                    Style::default().fg(theme::SUBTEXT0),
                ))),
                chunks[2],
            );
        }
    }
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
            Style::default().fg(theme::RED).add_modifier(Modifier::BOLD),
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
        (
            "GLOBAL",
            vec![
                ("q", "Quit"),
                ("`", "Switch account"),
                ("Ctrl+1-9", "Jump to account"),
                ("1-9", "Jump to mailbox"),
                ("Tab", "Cycle focus forward"),
                ("Shift+Tab", "Cycle focus backward"),
                ("/", "Filter by metadata"),
                ("\\", "Search email content"),
                ("?", "Toggle this help"),
                ("!", "Toggle activity log"),
                ("L", "Open activity log overlay"),
                ("Ctrl+l", "Open log file in $EDITOR"),
            ],
        ),
        (
            "SIDEBAR",
            vec![
                ("j/k", "Navigate mailboxes"),
                ("Enter", "Select mailbox"),
            ],
        ),
        (
            "EMAIL LIST",
            vec![
                ("j/k", "Navigate emails"),
                ("gg / G", "Jump to top / bottom"),
                ("Space", "Toggle selection"),
                ("Ctrl+a", "Select all visible"),
                ("Esc", "Clear selection"),
                ("Enter / e", "Open in editor"),
                ("r / R", "Reply / Reply-all"),
                ("w", "Forward"),
                ("a", "Archive"),
                ("d", "Delete"),
                ("m", "Toggle read/unread"),
                ("A", "Approve draft"),
                ("D", "Mark approved as draft (reverse A)"),
                ("x / X", "Send / Send all approved"),
                ("y", "Copy file path"),
                ("o", "Open attachment"),
                ("O", "Save attachment to disk"),
                ("b", "Open HTML in browser"),
                ("n", "New draft"),
                ("s / S", "Quick sync / Full sync"),
                ("f", "Search (IMAP)"),
            ],
        ),
        (
            "SERVER SEARCH",
            vec![
                ("j/k", "Navigate results"),
                ("gg / G", "Jump to top / bottom"),
                ("d/u", "Half-page down / up"),
                ("Enter / e", "Open in editor"),
                ("r / R", "Reply / Reply-all"),
                ("w", "Forward"),
                ("a", "Archive"),
                ("b", "Open HTML in browser"),
                ("o", "Open attachment"),
                ("O", "Save attachment to disk"),
                ("Tab", "Switch focus"),
                ("Esc", "Close overlay"),
            ],
        ),
        (
            "HEADERS",
            vec![("j/k", "Scroll headers")],
        ),
        (
            "BODY",
            vec![
                ("j/k", "Scroll line by line"),
                ("d/u", "Half-page down / up"),
                ("Esc", "Return to list"),
            ],
        ),
        (
            "ACTIVITY LOG",
            vec![
                ("j/k", "Scroll line by line"),
                ("d/u", "Half-page down / up"),
                ("gg / G", "Jump to top / bottom"),
                ("/", "Filter entries"),
                ("Esc", "Close overlay"),
            ],
        ),
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
            entries
                .iter()
                .filter(|(key, desc)| {
                    key.to_lowercase().contains(&filter) || desc.to_lowercase().contains(&filter)
                })
                .collect()
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
    let filter_bar_height: u16 = if show_filter_bar && inner_height >= 3 {
        1
    } else {
        0
    };
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

    let help = Paragraph::new(lines).scroll((app.help_scroll, 0));
    frame.render_widget(help, content_area);
}
