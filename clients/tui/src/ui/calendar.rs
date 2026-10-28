//! Calendar view rendering (#0034, PERSO-200): the events of every account in
//! three layouts on calcure's model (D10).
//!
//! - The agenda lists the days that have events, each event under every day
//!   it covers, all-day rows first, with its `account/calendar` tag.
//! - The day grid puts one day on an hour grid from 0 to 24, the all-day
//!   events in a band on top and overlapping events side by side when the
//!   width allows, else stacked one row under another.
//! - The week grid has seven columns, Monday to Sunday, the day numbers in
//!   the header with today highlighted, an all-day band, and the timed events
//!   as short `HH:MM summary` blocks; a pane too narrow for seven columns
//!   shows the day grid of the day under the cursor instead.
//!
//! Beside the agenda and the day grid sits the event card, which the
//! `Enter` overlay shows too. Everything here reads `App::calendar_view` and
//! draws; the model, the days and the cursor are `app::calendar`'s.

use chrono::{Datelike, NaiveDate, Timelike};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::Frame;

use mp_client::format::{calendar_names, calendar_tag, response_mark, CalendarNames};

use super::super::app::{
    day_part, App, CalendarEvent, CalendarEventTime, CalendarLayout, CalendarsOverlay, DayPart,
    EventCardOverlay, Tz,
};
use super::super::theme;
use super::util::{display_width, truncate};
use super::widgets::{modal_stack_areas, render_modal_shell};

/// Below this content width the agenda drops its event card.
const DETAIL_MIN_WIDTH: u16 = 56;

/// Below this content width the day grid drops its event card.
const DAY_DETAIL_MIN_WIDTH: u16 = 100;

/// Below this pane width the week grid falls back to the day grid: seven
/// columns of eleven cells, their six separators and the border.
const WEEK_MIN_WIDTH: u16 = 7 * 11 + 6 + 2;

/// Width of the agenda's time column (`HH:MM-HH:MM`, `all day`).
const TIME_WIDTH: usize = 11;

/// Width of the day grid's hour gutter (`all day` plus a space).
const GUTTER_WIDTH: u16 = 8;

/// The fewest cells an event block keeps side by side with another before
/// the day grid stacks overlapping events instead.
const LANE_MIN_WIDTH: u16 = 14;

/// The all-day band's most rows, the last one saying how many more there are.
const BAND_ROWS: usize = 3;

/// Render the whole Calendar view into `area`: the medium and narrow tiers.
pub(super) fn render_calendar(app: &App, frame: &mut Frame, area: Rect) {
    match app.calendar_view.layout {
        CalendarLayout::Agenda if area.width >= DETAIL_MIN_WIDTH => {
            let cols = split(area, 45);
            render_agenda(app, frame, cols[0]);
            render_detail(app, frame, cols[1]);
        }
        CalendarLayout::Agenda => render_agenda(app, frame, area),
        CalendarLayout::Day if area.width >= DAY_DETAIL_MIN_WIDTH => {
            let cols = split(area, 60);
            render_day(app, frame, cols[0], None);
            render_detail(app, frame, cols[1]);
        }
        CalendarLayout::Day => render_day(app, frame, area, None),
        CalendarLayout::Week => render_week(app, frame, area),
    }
}

/// Wide-tier entry point: the agenda owns the left column and the event
/// card the right one, the way the Mail list and preview do; a grid spans
/// both, with the event under the cursor in the strip the view switcher
/// leaves free on the right.
pub(super) fn render_calendar_split(
    app: &App,
    frame: &mut Frame,
    list_area: Rect,
    detail_area: Rect,
) {
    if app.calendar_view.layout == CalendarLayout::Agenda {
        render_agenda(app, frame, list_area);
        render_detail(app, frame, detail_area);
        return;
    }
    let grid = Rect {
        x: list_area.x,
        y: list_area.y,
        width: detail_area.right().saturating_sub(list_area.x),
        height: list_area.height,
    };
    if app.calendar_view.layout == CalendarLayout::Day {
        render_day(app, frame, grid, None);
    } else {
        render_week(app, frame, grid);
    }
    let strip = Rect {
        x: detail_area.x,
        y: grid.bottom(),
        width: detail_area.width,
        height: detail_area.bottom().saturating_sub(grid.bottom()),
    };
    render_selected_strip(app, frame, strip);
}

fn split(area: Rect, left_percent: u16) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(left_percent),
            Constraint::Percentage(100 - left_percent),
        ])
        .split(area)
}

// ---------------------------------------------------------------------------
// The pane every layout draws in
// ---------------------------------------------------------------------------

/// The re-login line for the accounts whose server calendar lacks the scope
/// (PERSO-200), the daemon's own sentence for one account.
fn scope_banner(app: &App) -> Option<String> {
    let missing = app.calendar_scope_missing();
    match missing.as_slice() {
        [] => None,
        [one] => Some(format!(
            "\u{26a0} {one}: calendar needs a re-login (mp config oauth2-login -A {one})"
        )),
        many => Some(format!(
            "\u{26a0} {}: calendar needs a re-login (mp config oauth2-login -A <name>)",
            many.join(", ")
        )),
    }
}

/// Draw the pane's border and title, the re-login banner on its first row and
/// the jump-to-date prompt on its last, and answer the area left between.
fn pane(app: &App, frame: &mut Frame, area: Rect, title: &str) -> Rect {
    let block = Block::default()
        .title(title.to_string())
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::active().border))
        .style(Style::default().bg(theme::active().bg));
    let mut inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return inner;
    }
    if let Some(banner) = scope_banner(app) {
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate(&banner, inner.width as usize),
                Style::default().fg(theme::active().warning),
            )),
            Rect { height: 1, ..inner },
        );
        inner.y += 1;
        inner.height -= 1;
    }
    if let Some(input) = &app.jump_date_input {
        if inner.height > 0 {
            let prompt = format!("Jump to date: {input}\u{258f}  (Enter go, Esc cancel)");
            frame.render_widget(
                Paragraph::new(Span::styled(
                    truncate(&prompt, inner.width as usize),
                    Style::default().fg(theme::active().accent),
                )),
                Rect {
                    y: inner.bottom() - 1,
                    height: 1,
                    ..inner
                },
            );
            inner.height -= 1;
        }
    }
    inner
}

/// A muted one-paragraph message filling `area`.
fn message(frame: &mut Frame, area: Rect, text: &str) {
    frame.render_widget(
        Paragraph::new(Span::styled(
            text.to_string(),
            Style::default().fg(theme::active().text_muted),
        ))
        .wrap(Wrap { trim: true }),
        area,
    );
}

/// What an empty or failed view says instead of its events, or `None`.
fn placeholder(app: &App, first: NaiveDate, last: NaiveDate) -> Option<String> {
    let cv = &app.calendar_view;
    if let Some(error) = &cv.error {
        return Some(format!("The daemon did not answer the calendar: {error}"));
    }
    if cv.entries.is_empty() {
        let span = if first == last {
            format!("on {}", first.format("%a %Y-%m-%d"))
        } else {
            format!("from {first} to {last}")
        };
        return Some(format!(
            "No events {span}. h/l move, t today, gt jump to a date, c calendars."
        ));
    }
    None
}

/// The cursor-row highlight, matching the Mail list convention (#TKT-0048).
fn cursor_style() -> Style {
    Style::default()
        .bg(theme::active().surface)
        .fg(theme::active().selection)
        .add_modifier(Modifier::BOLD)
}

/// An event's text style: struck through and muted once cancelled.
fn event_style(event: &CalendarEvent) -> Style {
    if event.cancelled {
        Style::default()
            .fg(theme::active().text_muted)
            .add_modifier(Modifier::CROSSED_OUT)
    } else {
        Style::default().fg(theme::active().text)
    }
}

fn title_of(event: &CalendarEvent) -> String {
    if event.summary.trim().is_empty() {
        event
            .source
            .as_ref()
            .map(|source| source.subject.clone())
            .unwrap_or_else(|| "(no title)".to_string())
    } else {
        event.summary.clone()
    }
}

/// A day's heading: `Mon 2026-07-06`, marked when it is today.
fn day_heading(day: NaiveDate, today: NaiveDate) -> String {
    let mut heading = day.format("%a %Y-%m-%d").to_string();
    if day == today {
        heading.push_str("  (today)");
    }
    heading
}

/// Write `text` at (`x`, `y`) clipped to `width` cells, then fill the rest
/// of the width with `style`.
fn put(frame: &mut Frame, x: u16, y: u16, width: u16, text: &str, style: Style) {
    if width == 0 {
        return;
    }
    let buf = frame.buffer_mut();
    buf.set_style(Rect::new(x, y, width, 1), style);
    buf.set_stringn(x, y, truncate(text, width as usize), width as usize, style);
}

// ---------------------------------------------------------------------------
// The agenda
// ---------------------------------------------------------------------------

fn render_agenda(app: &App, frame: &mut Frame, area: Rect) {
    let cv = &app.calendar_view;
    let (first, last) = cv.layout.days(cv.anchor);
    // The longest title that fits the border, down to the bare word.
    let room = area.width.saturating_sub(4) as usize;
    let title = [
        format!(
            " Agenda: {} to {} ",
            first.format("%a %d %b"),
            last.format("%a %d %b")
        ),
        format!(
            " Agenda: {} to {} ",
            first.format("%d %b"),
            last.format("%d %b")
        ),
        " Agenda ".to_string(),
    ]
    .into_iter()
    .find(|title| display_width(title) <= room)
    .unwrap_or_default();
    let inner = pane(app, frame, area, &title);
    if inner.height == 0 {
        return;
    }
    if let Some(text) = placeholder(app, first, last) {
        return message(frame, inner, &text);
    }
    let names = calendar_names(&cv.calendars);
    let width = inner.width as usize;
    // Every row, and which one the cursor is on.
    let mut lines: Vec<Line> = Vec::new();
    let mut cursor_line = 0usize;
    let mut previous: Option<NaiveDate> = None;
    for (index, entry) in cv.entries.iter().enumerate() {
        if previous != Some(entry.day) {
            if previous.is_some() {
                lines.push(Line::from(""));
            }
            let style = if entry.day == cv.today {
                Style::default()
                    .fg(theme::active().accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(theme::active().heading)
                    .add_modifier(Modifier::BOLD)
            };
            lines.push(Line::from(Span::styled(
                day_heading(entry.day, cv.today),
                style,
            )));
            previous = Some(entry.day);
        }
        let Some(event) = cv.events.get(entry.event) else {
            continue;
        };
        let time = day_part(event, entry.day, cv.zone)
            .map(|part| part.label())
            .unwrap_or_default();
        let selected = index == cv.list_index;
        lines.extend(agenda_lines(event, &time, &names, width, selected));
        if selected {
            cursor_line = lines.len() - 1;
        }
    }
    // Keep the cursor row in view, with its day's heading when it fits.
    let height = inner.height as usize;
    let offset = (cursor_line + 1).saturating_sub(height);
    frame.render_widget(
        Paragraph::new(lines.into_iter().skip(offset).collect::<Vec<_>>())
            .style(Style::default().bg(theme::active().bg)),
        inner,
    );
}

/// One event's agenda row: its time, its own-response mark and its title,
/// then its `account/calendar` tag and its place on the same line when the
/// pane is wide enough, else on a second line under the title.
fn agenda_lines(
    event: &CalendarEvent,
    time: &str,
    names: &CalendarNames,
    width: usize,
    selected: bool,
) -> Vec<Line<'static>> {
    let mut title = title_of(event);
    if event.cancelled {
        title.push_str(" (cancelled)");
    }
    let mut detail = format!("[{}]", calendar_tag(event, names));
    if let Some(location) = event.location.as_deref().filter(|l| !l.trim().is_empty()) {
        detail.push_str(&format!("  @ {location}"));
    }
    let lead = format!(
        "  {time:<TIME_WIDTH$} {} ",
        response_mark(&event.own_response)
    );
    let room = width.saturating_sub(display_width(&lead));
    let style = if selected {
        cursor_style()
    } else {
        event_style(event)
    };
    let faint = if selected {
        cursor_style()
    } else {
        Style::default().fg(theme::active().text_faint)
    };
    let one_line = display_width(&title) + 2 + display_width(&detail) <= room;
    if one_line {
        let pad = room - display_width(&title) - display_width(&detail);
        return vec![Line::from(vec![
            Span::styled(lead, style),
            Span::styled(title, style),
            Span::styled(" ".repeat(pad), style),
            Span::styled(detail, faint),
        ])];
    }
    let indent = " ".repeat(display_width(&lead));
    let detail = truncate(&detail, room);
    let pad = room.saturating_sub(display_width(&detail));
    let title = truncate(&title, room);
    let title_pad = room.saturating_sub(display_width(&title));
    vec![
        Line::from(vec![
            Span::styled(lead, style),
            Span::styled(title, style),
            Span::styled(" ".repeat(title_pad), style),
        ]),
        Line::from(vec![
            Span::styled(indent, style),
            Span::styled(detail, faint),
            Span::styled(" ".repeat(pad), style),
        ]),
    ]
}

// ---------------------------------------------------------------------------
// The day grid
// ---------------------------------------------------------------------------

/// One timed event on the day grid: its entry, and the hour rows it covers.
struct HourBlock {
    entry: usize,
    first_row: u32,
    end_row: u32,
    label: String,
}

/// The day grid of the anchor day, or of `day` when the week grid falls back
/// to it, titled `title` then.
fn render_day(app: &App, frame: &mut Frame, area: Rect, fallback: Option<(NaiveDate, String)>) {
    let cv = &app.calendar_view;
    let (day, title) = match fallback {
        Some(fallback) => fallback,
        None => (
            cv.anchor,
            format!(" Day: {} ", day_heading(cv.anchor, cv.today)),
        ),
    };
    let inner = pane(app, frame, area, &title);
    if inner.height == 0 || inner.width <= GUTTER_WIDTH {
        return;
    }
    if let Some(error) = &cv.error {
        return message(
            frame,
            inner,
            &format!("The daemon did not answer the calendar: {error}"),
        );
    }
    let on_day: Vec<usize> = (0..cv.entries.len())
        .filter(|&i| cv.entries[i].day == day)
        .collect();
    let part = |i: usize| {
        cv.events
            .get(cv.entries[i].event)
            .and_then(|event| day_part(event, day, cv.zone))
    };
    let all_day: Vec<usize> = on_day
        .iter()
        .copied()
        .filter(|&i| part(i) == Some(DayPart::AllDay))
        .collect();
    let timed: Vec<(usize, DayPart)> = on_day
        .iter()
        .filter_map(|&i| part(i).filter(|p| *p != DayPart::AllDay).map(|p| (i, p)))
        .collect();
    let muted = Style::default().fg(theme::active().text_muted);
    let mut y = inner.y;
    let grid_x = inner.x + GUTTER_WIDTH;
    let grid_w = inner.width - GUTTER_WIDTH;

    // The all-day band.
    if !all_day.is_empty() {
        let shown = band_rows(all_day.len());
        for (row, &entry) in all_day.iter().take(shown).enumerate() {
            if y >= inner.bottom() {
                return;
            }
            put(
                frame,
                inner.x,
                y,
                GUTTER_WIDTH,
                if row == 0 { "all day" } else { "" },
                muted,
            );
            let more = all_day.len() > BAND_ROWS && row + 1 == shown;
            if more {
                let text = format!("+{} more", all_day.len() - shown + 1);
                put(frame, grid_x, y, grid_w, &text, muted);
            } else {
                draw_block_row(
                    app,
                    frame,
                    entry,
                    grid_x,
                    y,
                    grid_w,
                    &title_of_entry(app, entry),
                );
            }
            y += 1;
        }
        if y < inner.bottom() {
            put(
                frame,
                inner.x,
                y,
                inner.width,
                &"\u{2500}".repeat(inner.width as usize),
                muted,
            );
            y += 1;
        }
    }
    let rows = inner.bottom().saturating_sub(y) as u32;
    if rows == 0 {
        return;
    }
    if timed.is_empty() && all_day.is_empty() {
        message(
            frame,
            Rect::new(grid_x, y, grid_w, 1),
            &format!("No events on {}.", day.format("%a %Y-%m-%d")),
        );
    }

    // The hour rows shown: all 24 when they fit, else a window that holds the
    // cursor's event, else the first event, else the working day.
    let shown = rows.min(24);
    let blocks: Vec<HourBlock> = timed
        .iter()
        .map(|(entry, part)| hour_block(*entry, part, day))
        .collect();
    let selected = cv.list_index;
    let focus_row = blocks
        .iter()
        .find(|block| block.entry == selected)
        .or_else(|| blocks.first())
        .map(|block| block.first_row)
        .unwrap_or(8);
    let first_hour = if shown >= 24 {
        0
    } else {
        focus_row.saturating_sub(1).min(24 - shown)
    };
    for row in 0..shown {
        let label = format!("{:02}:00", first_hour + row);
        put(frame, inner.x, y + row as u16, GUTTER_WIDTH, &label, muted);
    }

    // Lanes: overlapping events side by side when each keeps LANE_MIN_WIDTH
    // cells, else stacked one row under another.
    let mut lane_ends: Vec<u32> = Vec::new();
    let lanes: Vec<usize> = blocks
        .iter()
        .map(
            |block| match lane_ends.iter().position(|end| *end <= block.first_row) {
                Some(lane) => {
                    lane_ends[lane] = block.end_row;
                    lane
                }
                None => {
                    lane_ends.push(block.end_row);
                    lane_ends.len() - 1
                }
            },
        )
        .collect();
    let lane_count = lane_ends.len().max(1) as u16;
    let visible = |hour: u32| hour >= first_hour && hour < first_hour + shown;
    if lane_count == 1 || grid_w / lane_count >= LANE_MIN_WIDTH {
        let lane_w = grid_w / lane_count;
        for (block, lane) in blocks.iter().zip(&lanes) {
            let lane = *lane as u16;
            let x = grid_x + lane * lane_w;
            let width = if lane + 1 == lane_count {
                grid_w - lane * lane_w
            } else {
                lane_w - 1
            };
            let mut labelled = false;
            for hour in block.first_row..block.end_row {
                if !visible(hour) {
                    continue;
                }
                let row_y = y + (hour - first_hour) as u16;
                let text = if labelled {
                    String::new()
                } else {
                    format!("{} {}", block.label, title_of_entry(app, block.entry))
                };
                labelled = true;
                draw_block_row(app, frame, block.entry, x, row_y, width, &text);
            }
        }
    } else {
        let mut taken = vec![false; shown as usize];
        for block in &blocks {
            let from = block.first_row.max(first_hour);
            if !visible(from) && from >= first_hour + shown {
                continue;
            }
            let start = (from - first_hour) as usize;
            let Some(free) = (start..taken.len()).find(|&r| !taken[r]) else {
                continue;
            };
            taken[free] = true;
            let label = format!(
                "{} {}",
                part_label(app, block.entry, day),
                title_of_entry(app, block.entry)
            );
            draw_block_row(
                app,
                frame,
                block.entry,
                grid_x,
                y + free as u16,
                grid_w,
                &label,
            );
        }
    }
}

/// How many rows the all-day band takes for `count` events.
fn band_rows(count: usize) -> usize {
    count.min(BAND_ROWS)
}

/// The rows a timed event covers on the hour grid: from its start's hour to
/// the hour its end reaches into, at least one, 24 when it runs past midnight.
fn hour_block(entry: usize, part: &DayPart, day: NaiveDate) -> HourBlock {
    let DayPart::Timed {
        start,
        end,
        past_midnight,
        ..
    } = part
    else {
        return HourBlock {
            entry,
            first_row: 0,
            end_row: 1,
            label: String::new(),
        };
    };
    let first_row = if start.date_naive() < day {
        0
    } else {
        start.hour()
    };
    let end_row = if *past_midnight || end.date_naive() > day {
        24
    } else {
        end.hour() + u32::from(end.minute() > 0 || end.second() > 0)
    };
    HourBlock {
        entry,
        first_row,
        end_row: end_row.max(first_row + 1).min(24),
        label: part.start_clock(),
    }
}

fn title_of_entry(app: &App, entry: usize) -> String {
    let cv = &app.calendar_view;
    cv.entries
        .get(entry)
        .and_then(|entry| cv.events.get(entry.event))
        .map(title_of)
        .unwrap_or_default()
}

fn part_label(app: &App, entry: usize, day: NaiveDate) -> String {
    let cv = &app.calendar_view;
    cv.entries
        .get(entry)
        .and_then(|entry| cv.events.get(entry.event))
        .and_then(|event| day_part(event, day, cv.zone))
        .map(|part| part.label())
        .unwrap_or_default()
}

/// One row of an event block on a grid: an accent bar, then `text` (a
/// block's first row carries `HH:MM summary`, the rest only the bar); the
/// cursor's event in the cursor style.
fn draw_block_row(
    app: &App,
    frame: &mut Frame,
    entry: usize,
    x: u16,
    y: u16,
    width: u16,
    text: &str,
) {
    let cv = &app.calendar_view;
    let Some(event) = cv.entries.get(entry).and_then(|e| cv.events.get(e.event)) else {
        return;
    };
    if width == 0 {
        return;
    }
    let selected = entry == cv.list_index;
    let style = if selected {
        cursor_style()
    } else {
        event_style(event)
    };
    let bar_style = if selected {
        cursor_style()
    } else {
        Style::default().fg(theme::active().accent)
    };
    put(frame, x, y, 1, "\u{258e}", bar_style);
    put(frame, x + 1, y, width - 1, text, style);
}

// ---------------------------------------------------------------------------
// The week grid
// ---------------------------------------------------------------------------

fn render_week(app: &App, frame: &mut Frame, area: Rect) {
    let cv = &app.calendar_view;
    let (monday, sunday) = cv.layout.days(cv.anchor);
    let week = monday.iso_week().week();
    if area.width < WEEK_MIN_WIDTH {
        // Seven columns do not fit: the day grid of the day under the cursor.
        let day = app
            .selected_calendar_entry()
            .map(|entry| entry.day)
            .unwrap_or(cv.anchor);
        let title = format!(" Week {week}: {} ", day_heading(day, cv.today));
        return render_day(app, frame, area, Some((day, title)));
    }
    let title = format!(" Week {week}: {monday} to {sunday} ");
    let inner = pane(app, frame, area, &title);
    if inner.height < 3 {
        return;
    }
    if let Some(error) = &cv.error {
        return message(
            frame,
            inner,
            &format!("The daemon did not answer the calendar: {error}"),
        );
    }
    let muted = Style::default().fg(theme::active().text_muted);
    // Seven columns and six separators, the cells left over spread one
    // each over the first columns.
    let base = (inner.width - 6) / 7;
    let spare = (inner.width - 6) % 7;
    let width_of = |i: u16| base + u16::from(i < spare);
    let col_x = |i: u16| inner.x + (0..i).map(|j| width_of(j) + 1).sum::<u16>();
    let days: Vec<NaiveDate> = monday.iter_days().take(7).collect();
    let columns: Vec<(Vec<usize>, Vec<usize>)> = days
        .iter()
        .map(|day| {
            let mut all_day = Vec::new();
            let mut timed = Vec::new();
            for (i, entry) in cv.entries.iter().enumerate() {
                if entry.day != *day {
                    continue;
                }
                match cv
                    .events
                    .get(entry.event)
                    .and_then(|event| day_part(event, *day, cv.zone))
                {
                    Some(DayPart::AllDay) => all_day.push(i),
                    Some(_) => timed.push(i),
                    None => {}
                }
            }
            (all_day, timed)
        })
        .collect();

    // The header: the day names and numbers, today highlighted.
    let mut y = inner.y;
    for (i, day) in days.iter().enumerate() {
        let i = i as u16;
        let text = format!("{} {:02}", day.format("%a"), day.day());
        let style = if *day == cv.today {
            Style::default()
                .bg(theme::active().selection)
                .fg(theme::active().bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(theme::active().heading)
                .add_modifier(Modifier::BOLD)
        };
        let width = width_of(i);
        let pad = (width as usize).saturating_sub(display_width(&text)) / 2;
        put(
            frame,
            col_x(i),
            y,
            width,
            &format!("{}{text}", " ".repeat(pad)),
            style,
        );
    }
    y += 1;
    rule(frame, inner, y);
    y += 1;

    // The all-day band, as deep as the busiest day's all-day events.
    let band = columns
        .iter()
        .map(|(all_day, _)| band_rows(all_day.len()))
        .max()
        .unwrap_or(0) as u16;
    if band > 0 && y + band < inner.bottom() {
        for (i, (all_day, _)) in columns.iter().enumerate() {
            let i = i as u16;
            let shown = band_rows(all_day.len());
            for (row, &entry) in all_day.iter().take(shown).enumerate() {
                let row_y = y + row as u16;
                if all_day.len() > BAND_ROWS && row + 1 == shown {
                    let more = format!("+{} more", all_day.len() - shown + 1);
                    put(frame, col_x(i), row_y, width_of(i), &more, muted);
                } else {
                    let title = title_of_entry(app, entry);
                    draw_block_row(app, frame, entry, col_x(i), row_y, width_of(i), &title);
                }
            }
        }
        y += band;
        rule(frame, inner, y);
        y += 1;
    }

    // The timed events, one `HH:MM summary` row each, a column scrolled so
    // the cursor's event shows and its last row saying how many more there are.
    let rows = inner.bottom().saturating_sub(y) as usize;
    for (i, (_, timed)) in columns.iter().enumerate() {
        let i = i as u16;
        if rows == 0 {
            break;
        }
        let at = timed.iter().position(|&entry| entry == cv.list_index);
        let offset = at.map(|at| (at + 1).saturating_sub(rows)).unwrap_or(0);
        let rest = timed.len() - offset;
        let fits = rest <= rows;
        for (row, &entry) in timed.iter().skip(offset).take(rows).enumerate() {
            let row_y = y + row as u16;
            if !fits && row + 1 == rows && Some(offset + row) != at {
                let more = format!("+{} more", rest - row);
                put(frame, col_x(i), row_y, width_of(i), &more, muted);
                break;
            }
            let clock = part_clock(app, entry, days[i as usize]);
            let label = format!("{clock} {}", title_of_entry(app, entry));
            draw_block_row(app, frame, entry, col_x(i), row_y, width_of(i), &label);
        }
    }

    // The column separators, drawn last over every row.
    for i in 1..7u16 {
        let x = col_x(i) - 1;
        for row_y in inner.y..inner.bottom() {
            let cell = &mut frame.buffer_mut()[(x, row_y)];
            if cell.symbol() == "\u{2500}" {
                cell.set_symbol("\u{253c}");
            } else {
                cell.set_symbol("\u{2502}");
                cell.set_style(muted);
            }
        }
    }
}

/// A horizontal rule across the week grid at row `y`.
fn rule(frame: &mut Frame, inner: Rect, y: u16) {
    if y < inner.bottom() {
        put(
            frame,
            inner.x,
            y,
            inner.width,
            &"\u{2500}".repeat(inner.width as usize),
            Style::default().fg(theme::active().text_muted),
        );
    }
}

fn part_clock(app: &App, entry: usize, day: NaiveDate) -> String {
    let cv = &app.calendar_view;
    cv.entries
        .get(entry)
        .and_then(|entry| cv.events.get(entry.event))
        .and_then(|event| day_part(event, day, cv.zone))
        .map(|part| part.start_clock())
        .unwrap_or_default()
}

/// The week grid's strip under the right column on the wide tier: the event
/// under the cursor in one line.
fn render_selected_strip(app: &App, frame: &mut Frame, area: Rect) {
    if area.height == 0 || area.width < 4 {
        return;
    }
    let block = Block::default()
        .title(" Event ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::active().border))
        .style(Style::default().bg(theme::active().bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let cv = &app.calendar_view;
    let (Some(entry), Some(event)) = (app.selected_calendar_entry(), app.selected_event()) else {
        return message(frame, inner, "No event selected");
    };
    let time = day_part(event, entry.day, cv.zone)
        .map(|part| part.label())
        .unwrap_or_default();
    let line = format!(
        "{} {time} {} {}  Enter for the card",
        entry.day.format("%a %d"),
        response_mark(&event.own_response),
        title_of(event)
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate(&line, inner.width as usize),
            event_style(event),
        )),
        inner,
    );
}

// ---------------------------------------------------------------------------
// The event card
// ---------------------------------------------------------------------------

/// The card beside the agenda and the day grid.
fn render_detail(app: &App, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(" Event ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::active().border))
        .style(Style::default().bg(theme::active().bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(event) = app.selected_event() else {
        return message(frame, inner, "Select an event to see its details.");
    };
    let names = calendar_names(&app.calendar_view.calendars);
    frame.render_widget(
        Paragraph::new(event_lines(event, app.calendar_view.zone, &names))
            .wrap(Wrap { trim: false })
            .style(Style::default().bg(theme::active().bg)),
        inner,
    );
}

/// The `Enter` overlay: the same card, scrollable, with its keys.
pub(super) fn render_event_card(app: &App, card: &EventCardOverlay, frame: &mut Frame, area: Rect) {
    let names = calendar_names(&app.calendar_view.calendars);
    let lines = event_lines(&card.event, app.calendar_view.zone, &names);
    let width = area.width.saturating_sub(8).clamp(40, 96);
    // The card's lines, the border and the footer, up to the frame.
    let height = (lines.len() as u16 + 4).min(area.height.saturating_sub(4));
    let Some((_, inner)) = render_modal_shell(
        frame,
        area,
        width,
        height,
        theme::active().accent,
        theme::active().panel_bg,
    ) else {
        return;
    };
    let stack = modal_stack_areas(inner, 0, 1);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((card.scroll, 0)),
        stack.content,
    );
    if let Some(footer) = stack.footer {
        let keys = if card.event.source.is_some() {
            "  V respond \u{b7} m open the mail \u{b7} e open the .ics \u{b7} j/k scroll \u{b7} Esc close"
        } else {
            "  V respond \u{b7} j/k scroll \u{b7} Esc close"
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                keys,
                Style::default().fg(theme::active().text_muted),
            )),
            footer,
        );
    }
}

/// When an event happens, read in `zone`: `Tue 2026-07-07 09:00-10:00`, or
/// its two ends when it spans days, or its dates when it is all-day.
fn when(event: &CalendarEvent, zone: Tz) -> String {
    let date = |raw: &str| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok();
    let instant = |raw: &str| chrono::DateTime::parse_from_rfc3339(raw).ok();
    match &event.time {
        CalendarEventTime::AllDay { start, end } => {
            let (Some(first), Some(end)) = (date(start), date(end)) else {
                return format!("{start} to {end}, all day");
            };
            let last = end.pred_opt().filter(|last| *last > first).unwrap_or(first);
            if last == first {
                format!("{}, all day", first.format("%a %Y-%m-%d"))
            } else {
                format!(
                    "{} to {}, all day",
                    first.format("%a %Y-%m-%d"),
                    last.format("%a %Y-%m-%d")
                )
            }
        }
        CalendarEventTime::Timed { start, end, .. } => {
            let (Some(start), Some(end)) = (instant(start), instant(end)) else {
                return format!("{start} to {end}");
            };
            let (start, end) = (start.with_timezone(&zone), end.with_timezone(&zone));
            if start.date_naive() == end.date_naive() {
                format!(
                    "{}-{}",
                    start.format("%a %Y-%m-%d %H:%M"),
                    end.format("%H:%M")
                )
            } else {
                format!(
                    "{} to {}",
                    start.format("%a %Y-%m-%d %H:%M"),
                    end.format("%a %Y-%m-%d %H:%M")
                )
            }
        }
    }
}

fn person(name: Option<&str>, email: &str) -> String {
    match name.filter(|name| !name.trim().is_empty()) {
        Some(name) => format!("{name} <{email}>"),
        None => email.to_string(),
    }
}

fn status_style(status: &str) -> Style {
    let theme = theme::active();
    match status {
        "accepted" => Style::default().fg(theme.success),
        "declined" => Style::default().fg(theme.error),
        "tentative" => Style::default().fg(theme.warning),
        _ => Style::default().fg(theme.text_muted),
    }
}

/// The card's lines: the title, then when, where, the calendar, the
/// organizer, the attendees with their answers, the user's own answer, the
/// recurrence, the links, the invitation mail, and the body.
pub(super) fn event_lines(
    event: &CalendarEvent,
    zone: Tz,
    names: &CalendarNames,
) -> Vec<Line<'static>> {
    let theme = theme::active();
    let label = Style::default().fg(theme.text_muted);
    let value = Style::default().fg(theme.text);
    let mut title = vec![Span::styled(
        title_of(event),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )];
    if event.cancelled {
        title.push(Span::styled(
            "  cancelled by the organizer",
            Style::default().fg(theme.error),
        ));
    }
    let mut lines = vec![Line::from(title), Line::from("")];
    let field = |lines: &mut Vec<Line<'static>>, name: &str, text: String, style: Style| {
        lines.push(Line::from(vec![
            Span::styled(format!("{name:<11} "), label),
            Span::styled(text, style),
        ]));
    };
    field(&mut lines, "When", when(event, zone), value);
    if let Some(location) = event.location.as_deref().filter(|l| !l.trim().is_empty()) {
        field(&mut lines, "Where", location.to_string(), value);
    }
    field(&mut lines, "Calendar", calendar_tag(event, names), value);
    if let Some(organizer) = &event.organizer {
        field(
            &mut lines,
            "Organizer",
            person(organizer.name.as_deref(), &organizer.email),
            value,
        );
    }
    for (n, attendee) in event.attendees.iter().enumerate() {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<11} ", if n == 0 { "Attendees" } else { "" }),
                label,
            ),
            Span::styled(
                format!("{}  ", person(attendee.name.as_deref(), &attendee.email)),
                value,
            ),
            Span::styled(attendee.status.clone(), status_style(&attendee.status)),
        ]));
    }
    field(
        &mut lines,
        "Response",
        format!(
            "{} {}",
            response_mark(&event.own_response),
            event.own_response
        ),
        status_style(&event.own_response),
    );
    if let Some(recurrence) = event.recurrence.as_deref() {
        field(&mut lines, "Recurrence", recurrence.to_string(), value);
    } else if event.series_master_id.is_some() || event.recurrence_id.is_some() {
        field(
            &mut lines,
            "Recurrence",
            "one occurrence of a series".to_string(),
            value,
        );
    }
    if let Some(url) = event.online_meeting_url.as_deref() {
        field(&mut lines, "Online", url.to_string(), value);
    }
    if let Some(link) = event.web_link.as_deref() {
        field(&mut lines, "Link", link.to_string(), value);
    }
    if let Some(source) = &event.source {
        field(&mut lines, "Mail", source.subject.clone(), value);
    }
    if let Some(body) = event
        .body
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
    {
        lines.push(Line::from(""));
        lines.extend(body.lines().map(|line| Line::from(line.to_string())));
    }
    lines
}

// ---------------------------------------------------------------------------
// The calendars overlay
// ---------------------------------------------------------------------------

/// `c`: every calendar with its account and whether it is shown.
pub(super) fn render_calendars_overlay(overlay: &CalendarsOverlay, frame: &mut Frame, area: Rect) {
    let height = overlay.calendars.len() as u16 + 5;
    let Some((_, inner)) = render_modal_shell(
        frame,
        area,
        64,
        height,
        theme::active().accent,
        theme::active().panel_bg,
    ) else {
        return;
    };
    let stack = modal_stack_areas(inner, 2, 1);
    frame.render_widget(
        Paragraph::new(Span::styled(
            "Calendars",
            Style::default()
                .fg(theme::active().accent)
                .add_modifier(Modifier::BOLD),
        )),
        stack.header,
    );
    let width = stack.content.width as usize;
    let lines: Vec<Line> = overlay
        .calendars
        .iter()
        .enumerate()
        .map(|(i, calendar)| {
            let selected = i == overlay.selected;
            let marker = if selected { "\u{25b6} " } else { "  " };
            let check = if calendar.visible { "[x]" } else { "[ ]" };
            let mut text = format!("{marker}{check} {}/{}", calendar.account, calendar.name);
            if calendar.is_default {
                text.push_str("  (default)");
            }
            let style = if selected {
                Style::default()
                    .fg(theme::active().selection)
                    .add_modifier(Modifier::BOLD)
            } else if calendar.visible {
                Style::default().fg(theme::active().text)
            } else {
                Style::default().fg(theme::active().text_muted)
            };
            Line::from(Span::styled(truncate(&text, width), style))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), stack.content);
    if let Some(footer) = stack.footer {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "  Space show / hide \u{b7} Esc close",
                Style::default().fg(theme::active().text_muted),
            )),
            footer,
        );
    }
}
