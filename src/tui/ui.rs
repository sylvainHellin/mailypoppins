use ratatui::layout::{Alignment, Constraint, Direction, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap,
};
use ratatui::Frame;

use super::app::{App, Focus, StatusLevel};
use super::theme;

/// Render the entire UI from the current app state.
pub fn view(app: &mut App, frame: &mut Frame) {
    let area = frame.area();

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    let main_area = outer[0];
    let status_area = outer[1];

    let show_right = app.terminal_width >= 80;
    let show_sidebar = app.terminal_width >= 40;

    if show_right {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
            .split(main_area);

        let left_col = columns[0];
        let right_col = columns[1];

        let sidebar_height = (app.mailboxes.len() as u16) + 3;
        let left_panels = if app.show_activity_log {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(sidebar_height),
                    Constraint::Min(0),
                    Constraint::Length(6),
                ])
                .split(left_col)
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(sidebar_height), Constraint::Min(0)])
                .split(left_col)
        };

        render_sidebar(app, frame, left_panels[0]);
        render_email_list(app, frame, left_panels[1]);
        if app.show_activity_log && left_panels.len() > 2 {
            render_activity_log(app, frame, left_panels[2]);
        }

        let right_panels = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(sidebar_height), Constraint::Min(0)])
            .split(right_col);

        render_headers(app, frame, right_panels[0]);
        render_body(app, frame, right_panels[1]);
    } else if show_sidebar {
        let sidebar_height = (app.mailboxes.len() as u16) + 2;
        let left_panels = if app.show_activity_log {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(sidebar_height),
                    Constraint::Min(0),
                    Constraint::Length(6),
                ])
                .split(main_area)
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(sidebar_height), Constraint::Min(0)])
                .split(main_area)
        };

        render_sidebar(app, frame, left_panels[0]);
        render_email_list(app, frame, left_panels[1]);
        if app.show_activity_log && left_panels.len() > 2 {
            render_activity_log(app, frame, left_panels[2]);
        }
    } else {
        render_email_list(app, frame, main_area);
    }

    render_status_bar(app, frame, status_area);

    if let Some(dialog) = &app.confirm_dialog {
        render_confirm_dialog(dialog, frame, area);
    }

    if app.show_help {
        render_help_overlay(app, frame, area);
    }

    if app.show_search_overlay {
        render_search_overlay(app, frame, area);
    }

    if let Some(error) = &app.persistent_error {
        render_persistent_error(error, frame, area);
    }
}

fn render_sidebar(app: &App, frame: &mut Frame, area: Rect) {
    let border_style = pane_border_style(app.focus, Focus::Sidebar);
    let block = Block::default()
        .title(" Mail ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .style(Style::default().bg(theme::BASE));

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
                .fg(theme::GREEN)
                .add_modifier(Modifier::BOLD)
        } else if is_selected {
            Style::default().fg(theme::BLUE)
        } else {
            Style::default().fg(theme::TEXT)
        };

        lines.push(Line::from(Span::styled(label, style)));
    }

    let sidebar_content = Paragraph::new(lines);
    frame.render_widget(sidebar_content, inner);
}

fn render_activity_log(app: &App, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(" Activity ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::TEAL))
        .style(Style::default().bg(theme::BASE));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.status_log.is_empty() {
        let empty = Paragraph::new("  No activity yet")
            .style(Style::default().fg(theme::SUBTEXT0));
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
                StatusLevel::Success => theme::GREEN,
                StatusLevel::Error => theme::RED,
                StatusLevel::Warning => theme::YELLOW,
                StatusLevel::Info => theme::BLUE,
                StatusLevel::Progress => theme::TEAL,
            };
            Line::from(vec![
                Span::styled(format!(" {time} "), Style::default().fg(theme::OVERLAY0)),
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

fn render_email_list(app: &App, frame: &mut Frame, area: Rect) {
    let border_style = pane_border_style(app.focus, Focus::List);
    let title = if !app.search_query.is_empty() && app.focus != Focus::Search {
        if app.search_includes_body {
            format!(" {} (content search) ", app.active_label())
        } else {
            format!(" {} (filtered) ", app.active_label())
        }
    } else {
        format!(" {} ", app.active_label())
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .style(Style::default().bg(theme::BASE));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let search_visible = app.focus == Focus::Search || !app.search_query.is_empty();
    let (search_area, list_area) = if search_visible {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(inner);
        (Some(chunks[0]), chunks[1])
    } else {
        (None, inner)
    };

    if let Some(search_rect) = search_area {
        let prefix = if app.search_includes_body { "\\" } else { "/" };
        let mut spans = vec![
            Span::styled(prefix, Style::default().fg(theme::BLUE)),
            Span::styled(app.search_query.as_str(), Style::default().fg(theme::TEXT)),
        ];
        if app.focus == Focus::Search {
            spans.push(Span::styled("\u{2588}", Style::default().fg(theme::BLUE)));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), search_rect);
    }

    if app.emails.is_empty() {
        let msg = if !app.search_query.is_empty() {
            "  No matching emails".to_string()
        } else {
            format!(
                "\n  No emails in {}\n\n  Press f to fetch new emails",
                app.active_label()
            )
        };
        let empty = Paragraph::new(msg).style(Style::default().fg(theme::SUBTEXT0));
        frame.render_widget(empty, list_area);
        return;
    }

    let available_width = list_area.width as usize;
    let date_width = 10;
    let spacing = 3;

    let has_selection = !app.selection.is_empty();

    if available_width > 45 {
        let checkbox_extra: usize = if has_selection { 3 } else { 0 };
        let effective_width = available_width.saturating_sub(checkbox_extra);
        let contact_width = 15.min(effective_width.saturating_sub(date_width + spacing + 10));
        let subject_width = effective_width.saturating_sub(date_width + contact_width + spacing);

        let mut header_cells = Vec::new();
        if has_selection {
            header_cells.push(Cell::from("").style(Style::default().fg(theme::SUBTEXT0)));
        }
        header_cells.push(Cell::from("DATE").style(Style::default().fg(theme::SUBTEXT0)));
        header_cells.push(Cell::from("CONTACT").style(Style::default().fg(theme::SUBTEXT0)));
        header_cells.push(Cell::from("SUBJECT").style(Style::default().fg(theme::SUBTEXT0)));
        let header = Row::new(header_cells).height(1);

        let rows: Vec<Row> = app
            .emails
            .iter()
            .enumerate()
            .map(|(i, email)| {
                let is_cursor = i == app.list_index;
                let is_in_selection = has_selection && app.selection.contains(&email.path);
                let contact = truncate(email.display_contact(app.active_kind()), contact_width);
                let subject_prefix = if email.has_attachments { "\u{f0c6} " } else { "" };
                let subject = truncate(
                    &format!("{}{}", subject_prefix, email.subject),
                    subject_width,
                );

                let row_style = if is_cursor {
                    Style::default().bg(theme::SURFACE0).fg(theme::GREEN)
                } else if is_in_selection {
                    Style::default().bg(theme::SURFACE0).fg(theme::TEXT)
                } else {
                    Style::default().fg(theme::TEXT)
                };

                let mut cells = Vec::new();
                if has_selection {
                    let icon = if is_in_selection {
                        Span::styled("\u{f0134}", Style::default().fg(theme::GREEN))
                    } else {
                        Span::styled("\u{f0131}", Style::default().fg(theme::OVERLAY0))
                    };
                    cells.push(Cell::from(icon));
                }
                cells.push(Cell::from(email.date_display.clone()));
                cells.push(Cell::from(contact));
                cells.push(Cell::from(subject));

                Row::new(cells).style(row_style)
            })
            .collect();

        let mut constraints = Vec::new();
        if has_selection {
            constraints.push(Constraint::Length(2));
        }
        constraints.push(Constraint::Length(date_width as u16));
        constraints.push(Constraint::Length(contact_width as u16));
        constraints.push(Constraint::Min(subject_width as u16));

        let table = Table::new(rows, constraints)
        .header(header)
        .column_spacing(1)
        .row_highlight_style(
            Style::default()
                .bg(theme::SURFACE0)
                .fg(theme::GREEN)
                .add_modifier(Modifier::BOLD),
        );

        let mut state = TableState::default();
        state.select(Some(app.list_index));
        frame.render_stateful_widget(table, list_area, &mut state);
    } else {
        let checkbox_extra: usize = if has_selection { 3 } else { 0 };
        let subject_width = available_width.saturating_sub(date_width + 2 + checkbox_extra);

        let mut header_cells = Vec::new();
        if has_selection {
            header_cells.push(Cell::from("").style(Style::default().fg(theme::SUBTEXT0)));
        }
        header_cells.push(Cell::from("DATE").style(Style::default().fg(theme::SUBTEXT0)));
        header_cells.push(Cell::from("SUBJECT").style(Style::default().fg(theme::SUBTEXT0)));
        let header = Row::new(header_cells).height(1);

        let rows: Vec<Row> = app
            .emails
            .iter()
            .enumerate()
            .map(|(i, email)| {
                let is_cursor = i == app.list_index;
                let is_in_selection = has_selection && app.selection.contains(&email.path);
                let subject_prefix = if email.has_attachments { "\u{f0c6} " } else { "" };
                let subject = truncate(
                    &format!("{}{}", subject_prefix, email.subject),
                    subject_width,
                );

                let row_style = if is_cursor {
                    Style::default().bg(theme::SURFACE0).fg(theme::GREEN)
                } else if is_in_selection {
                    Style::default().bg(theme::SURFACE0).fg(theme::TEXT)
                } else {
                    Style::default().fg(theme::TEXT)
                };

                let mut cells = Vec::new();
                if has_selection {
                    let icon = if is_in_selection {
                        Span::styled("\u{f0134}", Style::default().fg(theme::GREEN))
                    } else {
                        Span::styled("\u{f0131}", Style::default().fg(theme::OVERLAY0))
                    };
                    cells.push(Cell::from(icon));
                }
                cells.push(Cell::from(email.date_display.clone()));
                cells.push(Cell::from(subject));

                Row::new(cells).style(row_style)
            })
            .collect();

        let mut constraints = Vec::new();
        if has_selection {
            constraints.push(Constraint::Length(2));
        }
        constraints.push(Constraint::Length(date_width as u16));
        constraints.push(Constraint::Min(subject_width as u16));

        let table = Table::new(rows, constraints)
        .header(header)
        .column_spacing(1)
        .row_highlight_style(
            Style::default()
                .bg(theme::SURFACE0)
                .fg(theme::GREEN)
                .add_modifier(Modifier::BOLD),
        );

        let mut state = TableState::default();
        state.select(Some(app.list_index));
        frame.render_stateful_widget(table, list_area, &mut state);
    }
}

fn header_line<'a>(label: &'a str, value: &'a str, color: Color) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!(" {label}: "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(value, Style::default().fg(color)),
    ])
}

fn render_headers(app: &App, frame: &mut Frame, area: Rect) {
    let border_style = pane_border_style(app.focus, Focus::Headers);
    let block = Block::default()
        .title(" Headers ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .style(Style::default().bg(theme::BASE));

    let selected = app.emails.get(app.list_index);
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

    lines.push(header_line("From", &email.from, theme::GREEN));
    lines.push(header_line("To", &email.to, theme::BLUE));
    if let Some(cc) = &email.cc {
        if !cc.is_empty() {
            lines.push(header_line("Cc", cc, theme::BLUE));
        }
    }
    lines.push(header_line("Subj", &email.subject, theme::YELLOW));

    let date_status = format!("{}  [{}]", email.date_display, email.status);
    lines.push(header_line("Date", &date_status, theme::MAUVE));

    let content = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.headers_scroll, 0));
    frame.render_widget(content, area);
}

fn render_body(app: &App, frame: &mut Frame, area: Rect) {
    let border_style = pane_border_style(app.focus, Focus::Preview);
    let block = Block::default()
        .title(" Body ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .style(Style::default().bg(theme::BASE));

    let selected = app.emails.get(app.list_index);
    if selected.is_none() {
        frame.render_widget(block, area);
        return;
    }

    let email = selected.unwrap();
    let body = email.body.replace("{{SIGNATURE}}", "[signature]");

    let inner_width = block.inner(area).width as usize;
    let lines: Vec<Line> = wrap_and_style_body(&body, inner_width);

    let content = Paragraph::new(lines)
        .block(block)
        .scroll((app.preview_scroll, 0));

    frame.render_widget(content, area);
}

fn parse_quote_depth(line: &str) -> (usize, &str) {
    let trimmed = line.trim_start();
    let mut depth = 0;
    let mut pos = 0;
    let bytes = trimmed.as_bytes();
    while pos < bytes.len() && bytes[pos] == b'>' {
        depth += 1;
        pos += 1;
        if pos < bytes.len() && bytes[pos] == b' ' {
            pos += 1;
        }
    }
    (depth, &trimmed[pos..])
}

fn parse_inline_markdown(text: &str, base_style: Style) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut plain = String::new();

    while i < len {
        if i + 1 < len && chars[i] == '*' && chars[i + 1] == '*' {
            if let Some(end) = find_closing(&chars, i + 2, &['*', '*']) {
                if !plain.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut plain), base_style));
                }
                let inner: String = chars[i + 2..end].iter().collect();
                spans.push(Span::styled(inner, base_style.add_modifier(Modifier::BOLD)));
                i = end + 2;
                continue;
            }
        }

        if chars[i] == '*' && (i + 1 >= len || chars[i + 1] != '*') {
            if let Some(end) = find_closing_single(&chars, i + 1, '*') {
                if !plain.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut plain), base_style));
                }
                let inner: String = chars[i + 1..end].iter().collect();
                spans.push(Span::styled(
                    inner,
                    base_style.add_modifier(Modifier::ITALIC),
                ));
                i = end + 1;
                continue;
            }
        }

        if chars[i] == '`' {
            if let Some(end) = find_closing_single(&chars, i + 1, '`') {
                if !plain.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut plain), base_style));
                }
                let inner: String = chars[i + 1..end].iter().collect();
                spans.push(Span::styled(
                    inner,
                    Style::default().fg(theme::PEACH).bg(theme::SURFACE0),
                ));
                i = end + 1;
                continue;
            }
        }

        if chars[i] == '[' {
            if let Some((link_text, end)) = parse_markdown_link(&chars, i) {
                if !plain.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut plain), base_style));
                }
                spans.push(Span::styled(
                    link_text,
                    Style::default()
                        .fg(theme::BLUE)
                        .add_modifier(Modifier::UNDERLINED),
                ));
                i = end;
                continue;
            }
        }

        plain.push(chars[i]);
        i += 1;
    }

    if !plain.is_empty() {
        spans.push(Span::styled(plain, base_style));
    }

    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base_style));
    }

    spans
}

fn find_closing(chars: &[char], start: usize, delim: &[char; 2]) -> Option<usize> {
    let len = chars.len();
    let mut i = start;
    while i + 1 < len {
        if chars[i] == delim[0] && chars[i + 1] == delim[1] && i > start {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn find_closing_single(chars: &[char], start: usize, delim: char) -> Option<usize> {
    (start..chars.len()).find(|&i| chars[i] == delim && i > start)
}

fn parse_markdown_link(chars: &[char], start: usize) -> Option<(String, usize)> {
    let len = chars.len();
    let mut i = start + 1;
    while i < len && chars[i] != ']' {
        i += 1;
    }
    if i >= len {
        return None;
    }
    let text: String = chars[start + 1..i].iter().collect();
    i += 1;
    if i >= len || chars[i] != '(' {
        return None;
    }
    let _paren_start = i + 1;
    while i < len && chars[i] != ')' {
        i += 1;
    }
    if i >= len {
        return None;
    }
    if text.is_empty() {
        return None;
    }
    Some((text, i + 1))
}

fn parse_heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let mut level = 0;
    for ch in trimmed.chars() {
        if ch == '#' {
            level += 1;
        } else {
            break;
        }
    }
    if level > 0 && level <= 6 {
        let rest = trimmed[level..].trim_start();
        if !rest.is_empty() || trimmed.len() > level {
            return Some((level, rest));
        }
    }
    None
}

fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.len() < 3 {
        return false;
    }
    let ch = trimmed.chars().next().unwrap();
    if ch == '-' || ch == '*' || ch == '_' {
        return trimmed.chars().all(|c| c == ch || c == ' ');
    }
    false
}

fn parse_list_item(line: &str) -> Option<(String, &str)> {
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();
    let pad: String = " ".repeat(indent);

    if trimmed.len() >= 2 {
        let first = trimmed.as_bytes()[0];
        if (first == b'-' || first == b'+') && trimmed.as_bytes()[1] == b' ' {
            return Some((format!("{}\u{2022} ", pad), &trimmed[2..]));
        }
    }

    let mut num_end = 0;
    for ch in trimmed.chars() {
        if ch.is_ascii_digit() {
            num_end += 1;
        } else {
            break;
        }
    }
    if num_end > 0 && trimmed.len() > num_end + 1 {
        let rest = &trimmed[num_end..];
        if let Some(stripped) = rest.strip_prefix(". ") {
            let number = &trimmed[..num_end];
            return Some((format!("{}{}. ", pad, number), stripped));
        }
    }

    None
}

fn wrap_and_style_body<'a>(body: &'a str, width: usize) -> Vec<Line<'a>> {
    let mut result: Vec<Line> = Vec::new();
    let mut in_code_block = false;

    for line in body.lines() {
        if line.trim() == "[signature]" {
            result.push(Line::from(Span::styled(
                "  -- signature --".to_string(),
                Style::default().fg(theme::OVERLAY0),
            )));
            continue;
        }

        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_block = !in_code_block;
            result.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(theme::OVERLAY0),
            )));
            continue;
        }

        if in_code_block {
            result.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(theme::PEACH).bg(theme::SURFACE0),
            )));
            continue;
        }

        let (depth, content) = parse_quote_depth(line);

        if depth == 0 {
            if is_horizontal_rule(content) {
                let rule: String = "\u{2500}".repeat(width.min(40));
                result.push(Line::from(Span::styled(
                    rule,
                    Style::default().fg(theme::OVERLAY0),
                )));
                continue;
            }

            if let Some((level, heading_text)) = parse_heading(content) {
                let style = match level {
                    1 => Style::default()
                        .fg(theme::MAUVE)
                        .add_modifier(Modifier::BOLD),
                    2 => Style::default()
                        .fg(theme::BLUE)
                        .add_modifier(Modifier::BOLD),
                    _ => Style::default()
                        .fg(theme::TEAL)
                        .add_modifier(Modifier::BOLD),
                };
                for wrapped in word_wrap(heading_text, width) {
                    result.push(Line::from(parse_inline_markdown(&wrapped, style)));
                }
                continue;
            }

            if let Some((prefix, item_content)) = parse_list_item(content) {
                let prefix_width = prefix.chars().count();
                let text_width = width.saturating_sub(prefix_width);
                let base_style = Style::default().fg(theme::TEXT);
                if text_width < 5 {
                    let mut spans = vec![Span::styled(prefix, Style::default().fg(theme::BLUE))];
                    spans.extend(parse_inline_markdown(item_content, base_style));
                    result.push(Line::from(spans));
                } else {
                    let wrapped_lines = word_wrap(item_content, text_width);
                    for (i, wrapped) in wrapped_lines.iter().enumerate() {
                        let line_prefix = if i == 0 {
                            prefix.clone()
                        } else {
                            " ".repeat(prefix_width)
                        };
                        let mut spans =
                            vec![Span::styled(line_prefix, Style::default().fg(theme::BLUE))];
                        spans.extend(parse_inline_markdown(wrapped, base_style));
                        result.push(Line::from(spans));
                    }
                }
                continue;
            }

            if is_attribution(content.trim()) {
                let style = Style::default()
                    .fg(theme::SUBTEXT0)
                    .add_modifier(Modifier::ITALIC);
                for wrapped in word_wrap(content, width) {
                    result.push(Line::from(Span::styled(wrapped, style)));
                }
            } else {
                let base_style = Style::default().fg(theme::TEXT);
                for wrapped in word_wrap(content, width) {
                    result.push(Line::from(parse_inline_markdown(&wrapped, base_style)));
                }
            }
        } else {
            let prefix = "\u{2502} ".repeat(depth);
            let prefix_width = depth * 2;
            let text_width = width.saturating_sub(prefix_width);

            let is_attr = is_attribution(content.trim());
            let text_style = if is_attr {
                Style::default()
                    .fg(theme::SUBTEXT0)
                    .add_modifier(Modifier::ITALIC)
            } else {
                match depth {
                    1 => Style::default().fg(theme::OVERLAY0),
                    _ => Style::default().fg(theme::SURFACE0),
                }
            };

            if text_width < 5 {
                let mut spans = vec![Span::styled(prefix, Style::default().fg(theme::BLUE))];
                if is_attr {
                    spans.push(Span::styled(content.to_string(), text_style));
                } else {
                    spans.extend(parse_inline_markdown(content, text_style));
                }
                result.push(Line::from(spans));
            } else {
                for wrapped in word_wrap(content, text_width) {
                    let mut spans = vec![Span::styled(
                        prefix.clone(),
                        Style::default().fg(theme::BLUE),
                    )];
                    if is_attr {
                        spans.push(Span::styled(wrapped, text_style));
                    } else {
                        spans.extend(parse_inline_markdown(&wrapped, text_style));
                    }
                    result.push(Line::from(spans));
                }
            }
        }
    }

    result
}

fn is_attribution(line: &str) -> bool {
    line.starts_with("On ") && line.ends_with("wrote:")
}

fn word_wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    if text.is_empty() {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        let char_count = remaining.chars().count();
        if char_count <= width {
            lines.push(remaining.to_string());
            break;
        }

        let byte_at_width: usize = remaining
            .char_indices()
            .nth(width)
            .map_or(remaining.len(), |(i, _)| i);

        let break_at = remaining[..byte_at_width]
            .rfind(' ')
            .map(|i| i + 1)
            .unwrap_or(byte_at_width);

        let (chunk, rest) = remaining.split_at(break_at);
        lines.push(chunk.trim_end().to_string());
        remaining = rest.trim_start();
    }

    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn render_status_bar(app: &App, frame: &mut Frame, area: Rect) {
    let total = app.mailbox_counts[app.active_mailbox];
    let shown = app.emails.len();
    let watch_prefix = if app.watcher_active { "WATCHING " } else { "" };
    let sel_text = if app.selection.is_empty() {
        String::new()
    } else {
        format!("{} sel | ", app.selection.len())
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
    let right_len = (sel_text.len() + watch_prefix.len() + mailbox_text.len() + 1) as u16;

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(right_len)])
        .split(area);

    let left_content = if app.bg_count > 0 {
        let frames = ['\u{280b}', '\u{2819}', '\u{2838}', '\u{2834}', '\u{2826}', '\u{2807}'];
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
        match app.focus {
            Focus::Sidebar => Line::from(vec![
                hint_span(" j/k"),
                desc_span("nav "),
                hint_span("Enter"),
                desc_span("select "),
                hint_span("/"),
                desc_span("search "),
                hint_span("!"),
                desc_span("log "),
                hint_span("?"),
                desc_span("help "),
                hint_span("q"),
                desc_span("quit"),
            ]),
            Focus::List => Line::from(vec![
                hint_span(" Space"),
                desc_span("sel "),
                hint_span("e"),
                desc_span("edit "),
                hint_span("r"),
                desc_span("reply "),
                hint_span("w"),
                desc_span("fwd "),
                hint_span("a"),
                desc_span("archive "),
                hint_span("A"),
                desc_span("approve "),
                hint_span("x"),
                desc_span("send "),
                hint_span("n"),
                desc_span("new "),
                hint_span("/"),
                desc_span("filter "),
                hint_span("\\"),
                desc_span("search "),
                hint_span("!"),
                desc_span("log "),
                hint_span("?"),
                desc_span("help"),
            ]),
            Focus::Headers => Line::from(vec![
                hint_span(" j/k"),
                desc_span("scroll "),
                hint_span("h"),
                desc_span("back "),
                hint_span("l"),
                desc_span("body "),
                hint_span("?"),
                desc_span("help "),
                hint_span("q"),
                desc_span("quit"),
            ]),
            Focus::Preview => Line::from(vec![
                hint_span(" j/k"),
                desc_span("scroll "),
                hint_span("d/u"),
                desc_span("page "),
                hint_span("h"),
                desc_span("back "),
                hint_span("/"),
                desc_span("search "),
                hint_span("?"),
                desc_span("help "),
                hint_span("q"),
                desc_span("quit"),
            ]),
            Focus::Search => {
                let mut spans = vec![
                    hint_span(" Enter"),
                    desc_span("confirm "),
                    hint_span("Esc"),
                    desc_span("cancel"),
                ];
                if app.search_includes_body {
                    spans.push(desc_span(" (content search)"));
                }
                Line::from(spans)
            }
        }
    };

    let left = Paragraph::new(left_content)
        .style(Style::default().fg(theme::SUBTEXT0).bg(theme::SURFACE0));
    frame.render_widget(left, chunks[0]);

    let mut right_spans = vec![Span::styled(" ", Style::default())];
    if !sel_text.is_empty() {
        right_spans.push(Span::styled(sel_text, Style::default().fg(theme::YELLOW)));
    }
    if app.watcher_active {
        right_spans.push(Span::styled(watch_prefix, Style::default().fg(theme::TEAL)));
    }
    right_spans.push(Span::styled(mailbox_text, Style::default().fg(theme::BLUE)));
    let right = Paragraph::new(Line::from(right_spans))
        .style(Style::default().bg(theme::SURFACE0))
        .alignment(Alignment::Right);
    frame.render_widget(right, chunks[1]);
}

fn render_confirm_dialog(dialog: &super::app::ConfirmDialog, frame: &mut Frame, area: Rect) {
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

fn render_persistent_error(error: &super::app::PersistentError, frame: &mut Frame, area: Rect) {
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

fn hint_span(key: &str) -> Span<'_> {
    Span::styled(key, Style::default().fg(theme::BLUE))
}

fn desc_span(desc: &str) -> Span<'_> {
    Span::styled(desc, Style::default().fg(theme::SUBTEXT0))
}

fn truncate(s: &str, max_width: usize) -> String {
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

// ---------------------------------------------------------------------------
// Server search overlay
// ---------------------------------------------------------------------------

fn render_search_overlay(app: &mut App, frame: &mut Frame, area: Rect) {
    let overlay_width = (area.width * 9 / 10).max(40).min(area.width.saturating_sub(4));
    let overlay_height = (area.height * 85 / 100).max(15).min(area.height.saturating_sub(2));

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

    // Clear the entire background so the main panes don't bleed through
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default().style(Style::default().bg(theme::BASE)),
        area,
    );

    let overlay_area = vertical[0];

    let mailbox_label = &app.server_search_scope_label;

    let bottom_title = if let Some(ref status) = app.server_search_status {
        format!(" {} ", status)
    } else {
        " Enter: search | Tab: switch focus | Esc: close ".to_string()
    };

    let border_color = theme::TEAL;
    let block = Block::default()
        .title(format!(" Server Search ({}) ", mailbox_label))
        .title_bottom(Line::from(bottom_title).alignment(Alignment::Center))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(theme::BASE));

    let inner = block.inner(overlay_area);
    frame.render_widget(block, overlay_area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // search input + hints
            Constraint::Min(0),   // results
        ])
        .split(inner);

    let search_area = chunks[0];
    let results_area = chunks[1];

    render_search_input(app, frame, search_area);

    if app.server_search_results.is_empty() {
        let msg = if app.server_search_loading {
            "  Searching..."
        } else if app.server_search_query.is_empty() {
            "  Type a query and press Enter to search"
        } else {
            "  No results"
        };
        let empty = Paragraph::new(msg).style(Style::default().fg(theme::SUBTEXT0));
        frame.render_widget(empty, results_area);
        return;
    }

    let show_right = results_area.width >= 60;
    if show_right {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(results_area);
        render_search_results_list(app, frame, cols[0]);

        let right_panels = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(6), // headers
                Constraint::Min(0),   // body
            ])
            .split(cols[1]);
        render_search_result_headers(app, frame, right_panels[0]);
        render_search_result_body(app, frame, right_panels[1]);
    } else {
        render_search_results_list(app, frame, results_area);
    }
}

fn render_search_input(app: &App, frame: &mut Frame, area: Rect) {
    let input_focus = app.server_search_focus == super::app::SearchOverlayFocus::Input;

    let input_area = Rect { height: 1, ..area };
    let mut spans = vec![
        Span::styled("> ", Style::default().fg(theme::TEAL)),
        Span::styled(
            app.server_search_query.as_str(),
            Style::default().fg(theme::TEXT),
        ),
    ];
    if input_focus {
        spans.push(Span::styled("\u{2588}", Style::default().fg(theme::TEAL)));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), input_area);

    if area.height >= 2 {
        let hints_area = Rect {
            y: area.y + 1,
            height: 1,
            ..area
        };
        let hints = Line::from(vec![
            Span::styled("from:", Style::default().fg(theme::BLUE)),
            Span::styled(" to:", Style::default().fg(theme::BLUE)),
            Span::styled(" subject:", Style::default().fg(theme::BLUE)),
            Span::styled(" body:", Style::default().fg(theme::BLUE)),
            Span::styled(" since:", Style::default().fg(theme::BLUE)),
            Span::styled(" before:", Style::default().fg(theme::BLUE)),
            Span::styled(" in:", Style::default().fg(theme::BLUE)),
            Span::styled("  or free text", Style::default().fg(theme::OVERLAY0)),
        ]);
        frame.render_widget(Paragraph::new(hints), hints_area);
    }
}

fn render_search_results_list(app: &App, frame: &mut Frame, area: Rect) {
    let list_focus = app.server_search_focus == super::app::SearchOverlayFocus::List;
    let show_mailbox_col = app.server_search_scope_label == "All";

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(if list_focus { theme::TEAL } else { theme::OVERLAY0 }));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let available_width = inner.width as usize;
    let date_width = 10;
    let mb_width = if show_mailbox_col { 8 } else { 0 };
    let contact_width = 15.min(available_width.saturating_sub(date_width + mb_width + 10));
    let subject_width =
        available_width.saturating_sub(date_width + mb_width + contact_width + 2 + if show_mailbox_col { 1 } else { 0 });

    let mut header_cells = vec![
        Cell::from("DATE").style(Style::default().fg(theme::SUBTEXT0)),
    ];
    if show_mailbox_col {
        header_cells.push(Cell::from("MAILBOX").style(Style::default().fg(theme::SUBTEXT0)));
    }
    header_cells.push(Cell::from("CONTACT").style(Style::default().fg(theme::SUBTEXT0)));
    header_cells.push(Cell::from("SUBJECT").style(Style::default().fg(theme::SUBTEXT0)));
    let header = Row::new(header_cells);

    let rows: Vec<Row> = app
        .server_search_results
        .iter()
        .enumerate()
        .map(|(i, result)| {
            let is_cursor = list_focus && i == app.server_search_index;
            let contact = truncate(&result.entry.from, contact_width);
            let subject_prefix = if result.entry.has_attachments {
                "\u{f0c6} "
            } else {
                ""
            };
            let subject = truncate(
                &format!("{}{}", subject_prefix, result.entry.subject),
                subject_width,
            );

            let style = if is_cursor {
                Style::default().bg(theme::SURFACE0).fg(theme::GREEN)
            } else {
                Style::default().fg(theme::TEXT)
            };

            let mut cells = vec![Cell::from(result.entry.date_display.clone())];
            if show_mailbox_col {
                cells.push(
                    Cell::from(truncate(&result.source_label, mb_width))
                        .style(Style::default().fg(theme::OVERLAY0)),
                );
            }
            cells.push(Cell::from(contact));
            cells.push(Cell::from(subject));

            Row::new(cells).style(style)
        })
        .collect();

    let mut constraints: Vec<Constraint> = vec![Constraint::Length(date_width as u16)];
    if show_mailbox_col {
        constraints.push(Constraint::Length(mb_width as u16));
    }
    constraints.push(Constraint::Length(contact_width as u16));
    constraints.push(Constraint::Min(subject_width as u16));

    let table = Table::new(rows, constraints)
        .header(header)
        .column_spacing(1);

    let mut state = TableState::default();
    if list_focus {
        state.select(Some(app.server_search_index));
    }
    frame.render_stateful_widget(table, inner, &mut state);
}

fn render_search_result_headers(app: &App, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(" Headers ")
        .borders(Borders::TOP | Borders::LEFT)
        .border_style(Style::default().fg(theme::OVERLAY0))
        .style(Style::default().bg(theme::BASE));

    let result = app.server_search_results.get(app.server_search_index);
    if result.is_none() {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new("  No email selected").style(Style::default().fg(theme::SUBTEXT0)),
            inner,
        );
        return;
    }

    let entry = &result.unwrap().entry;
    let mut lines: Vec<Line> = vec![
        header_line("From", &entry.from, theme::GREEN),
        header_line("To", &entry.to, theme::BLUE),
    ];
    if let Some(ref cc) = entry.cc {
        if !cc.is_empty() {
            lines.push(header_line("Cc", cc, theme::BLUE));
        }
    }
    lines.push(header_line("Subject", &entry.subject, theme::YELLOW));
    lines.push(header_line("Date", &entry.date_display, theme::MAUVE));

    let content = Paragraph::new(lines)
        .block(block)
        .scroll((app.server_search_headers_scroll, 0));

    frame.render_widget(content, area);
}

fn render_search_result_body(app: &App, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(" Body ")
        .borders(Borders::TOP | Borders::LEFT)
        .border_style(Style::default().fg(theme::OVERLAY0))
        .style(Style::default().bg(theme::BASE));

    let result = app.server_search_results.get(app.server_search_index);
    if result.is_none() {
        frame.render_widget(block, area);
        return;
    }

    let entry = &result.unwrap().entry;
    let content = Paragraph::new(entry.body.as_str())
        .style(Style::default().fg(theme::TEXT))
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.server_search_scroll, 0));

    frame.render_widget(content, area);
}

fn help_sections() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    vec![
        ("GLOBAL", vec![
            ("q", "Quit"),
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

fn render_help_overlay(app: &mut App, frame: &mut Frame, area: Rect) {
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

    // Build filtered content
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

    // Determine if filter bar is shown
    let show_filter_bar = app.help_filter_active || !app.help_filter.is_empty();
    let inner_height = help_area.height.saturating_sub(2); // subtract top + bottom border
    let filter_bar_height: u16 = if show_filter_bar && inner_height >= 3 { 1 } else { 0 };
    let visible_height = inner_height.saturating_sub(filter_bar_height);

    // Clamp scroll
    let max_scroll = content_height.saturating_sub(visible_height);
    app.help_scroll = app.help_scroll.min(max_scroll);

    // Bottom title: scroll position or hints
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

    // Render filter bar if visible
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

fn pane_border_style(current_focus: Focus, pane: Focus) -> Style {
    let focused = current_focus == pane || (current_focus == Focus::Search && pane == Focus::List);
    if focused {
        Style::default().fg(theme::PEACH)
    } else {
        Style::default().fg(theme::BLUE)
    }
}
