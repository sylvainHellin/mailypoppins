use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

use super::super::app::{App, Focus};
use super::super::theme;
use super::util::pane_border_style;

pub(super) fn render_body(app: &App, frame: &mut Frame, area: Rect) {
    let border_style = pane_border_style(app.focus, Focus::Preview);
    let block = Block::default()
        .title(" Body ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .style(Style::default().bg(theme::active().bg));

    let selected = app.selected_email();
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
                    Style::default()
                        .fg(theme::active().code)
                        .bg(theme::active().surface),
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
                        .fg(theme::active().accent)
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
                Style::default().fg(theme::active().text_faint),
            )));
            continue;
        }

        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_block = !in_code_block;
            result.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(theme::active().text_faint),
            )));
            continue;
        }

        if in_code_block {
            result.push(Line::from(Span::styled(
                line.to_string(),
                Style::default()
                    .fg(theme::active().code)
                    .bg(theme::active().surface),
            )));
            continue;
        }

        let (depth, content) = parse_quote_depth(line);

        if depth == 0 {
            if is_horizontal_rule(content) {
                let rule: String = "\u{2500}".repeat(width.min(40));
                result.push(Line::from(Span::styled(
                    rule,
                    Style::default().fg(theme::active().text_faint),
                )));
                continue;
            }

            if let Some((level, heading_text)) = parse_heading(content) {
                let style = match level {
                    1 => Style::default()
                        .fg(theme::active().heading)
                        .add_modifier(Modifier::BOLD),
                    2 => Style::default()
                        .fg(theme::active().accent)
                        .add_modifier(Modifier::BOLD),
                    _ => Style::default()
                        .fg(theme::active().accent_alt)
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
                let base_style = Style::default().fg(theme::active().text);
                if text_width < 5 {
                    let mut spans = vec![Span::styled(
                        prefix,
                        Style::default().fg(theme::active().accent),
                    )];
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
                        let mut spans = vec![Span::styled(
                            line_prefix,
                            Style::default().fg(theme::active().accent),
                        )];
                        spans.extend(parse_inline_markdown(wrapped, base_style));
                        result.push(Line::from(spans));
                    }
                }
                continue;
            }

            if is_attribution(content.trim()) {
                let style = Style::default()
                    .fg(theme::active().text_muted)
                    .add_modifier(Modifier::ITALIC);
                for wrapped in word_wrap(content, width) {
                    result.push(Line::from(Span::styled(wrapped, style)));
                }
            } else {
                let base_style = Style::default().fg(theme::active().text);
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
                    .fg(theme::active().text_muted)
                    .add_modifier(Modifier::ITALIC)
            } else {
                match depth {
                    1 => Style::default().fg(theme::active().text_faint),
                    _ => Style::default().fg(theme::active().surface),
                }
            };

            if text_width < 5 {
                let mut spans = vec![Span::styled(
                    prefix,
                    Style::default().fg(theme::active().accent),
                )];
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
                        Style::default().fg(theme::active().accent),
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
