//! The event form's availability rows (PERSO-255): one row of glyphs per
//! person and room over the form's day from 07:00 to 20:00, one character
//! per 30-minute slot (`·` free, `▒` tentative, `█` busy, `▓` away, `░`
//! working elsewhere, `?` unknown), the chosen time's slots underlined, and
//! "no free/busy" for an address the server cannot read. An edit form's
//! rows also colour the slots the event holds now, so the organizer's busy
//! block there reads as the event itself, with a legend beside the ruler.

use chrono::{NaiveDateTime, NaiveTime, TimeZone, Utc};
use mp_client::format::{availability_row, slot_of};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::util::truncate;
use crate::app::{Availability, EventForm};
use crate::theme;

/// The first hour a row shows.
const FIRST_HOUR: u32 = 7;
/// The hour a row ends at.
const LAST_HOUR: u32 = 20;
/// The label column's width.
const LABEL: usize = 22;
/// The most rows shown however tall the terminal; the rest are counted.
const MAX_ROWS: usize = 8;

/// The hour ruler over the rows: a label every two hours.
fn ruler(per_hour: usize) -> String {
    let mut out = String::new();
    for hour in (FIRST_HOUR..LAST_HOUR).step_by(2) {
        out.push_str(&format!("{hour:02}"));
        out.push_str(&" ".repeat((2 * per_hour).saturating_sub(2)));
    }
    out.trim_end().to_string()
}

/// `address` in the label column, cut with an ellipsis.
fn label(address: &str) -> String {
    let count = address.chars().count();
    let shown = if count > LABEL {
        let mut cut: String = address.chars().take(LABEL - 1).collect();
        cut.push('\u{2026}');
        cut
    } else {
        address.to_string()
    };
    format!("  {shown:<LABEL$} ")
}

/// The slots of the form's chosen time, as offsets from the row's first
/// slot, on a timed event of the shown day.
fn chosen(form: &EventForm, first: i64, start: &str, interval: u32) -> Option<(i64, i64)> {
    if form.all_day {
        return None;
    }
    let zone = mp_core::calendar_model::parse_zone(&form.zone)?;
    let date = chrono::NaiveDate::parse_from_str(form.date.trim(), "%Y-%m-%d").ok()?;
    let from = NaiveTime::parse_from_str(form.start.trim(), "%H:%M").ok()?;
    let end = form.end.trim();
    let to = match NaiveTime::parse_from_str(end, "%H:%M") {
        Ok(at) => date.and_time(at),
        Err(_) => NaiveDateTime::parse_from_str(end, "%Y-%m-%dT%H:%M").ok()?,
    };
    let instant = |local: NaiveDateTime| {
        zone.from_local_datetime(&local)
            .earliest()
            .map(|at| at.with_timezone(&Utc))
    };
    let (from, to) = (instant(date.and_time(from))?, instant(to)?);
    if to <= from {
        return None;
    }
    let first_slot = slot_of(start, interval, from)? - first;
    let last_slot = slot_of(start, interval, to - chrono::Duration::seconds(1))? - first;
    Some((first_slot, last_slot))
}

/// The lines the form shows under its fields, at most `max` of them in
/// `width` columns: none while the form's account has no free/busy, else
/// the header and a row per schedule. Rows that do not fit are counted on
/// a "+N more" line, so a short terminal keeps the form's problem line.
pub(super) fn availability_lines(
    form: &EventForm,
    state: &Availability,
    width: usize,
    max: usize,
) -> Vec<Line<'static>> {
    if state.seen.is_none() || max == 0 {
        return Vec::new();
    }
    let muted = Style::default().fg(theme::active().text_muted);
    let checking = if state.checking() {
        "  checking\u{2026}"
    } else {
        ""
    };
    let answer = match &state.answer {
        None => {
            return vec![Line::from(Span::styled(
                format!("{}{}", label("Availability"), checking.trim_start()),
                muted,
            ))];
        }
        Some(Err(why)) => {
            let room = width.saturating_sub(label("Availability").chars().count());
            return vec![Line::from(vec![
                Span::styled(label("Availability"), muted),
                Span::styled(
                    truncate(&format!("unavailable: {why}"), room),
                    Style::default().fg(theme::active().warning),
                ),
            ])];
        }
        Some(Ok(answer)) => answer,
    };
    let interval = answer.interval_minutes.max(1);
    let per_hour = (60 / interval).max(1) as usize;
    let slots = (LAST_HOUR - FIRST_HOUR) as usize * per_hour;
    let zone = mp_core::calendar_model::parse_zone(&form.zone);
    let date = chrono::NaiveDate::parse_from_str(form.date.trim(), "%Y-%m-%d").ok();
    let first = zone
        .zip(date)
        .and_then(|(zone, date)| {
            zone.from_local_datetime(&date.and_hms_opt(FIRST_HOUR, 0, 0)?)
                .earliest()
        })
        .and_then(|at| slot_of(&answer.start, interval, at.with_timezone(&Utc)));
    let Some(first) = first else {
        return Vec::new();
    };
    let total = answer.schedules.len();
    // The header, then as many rows as fit with room for the count of the
    // rest; one line alone is the header's label and that count.
    let fits = |rows: usize| 1 + rows + usize::from(rows < total) <= max;
    let shown = (0..=total.min(MAX_ROWS))
        .rev()
        .find(|rows| fits(*rows))
        .unwrap_or(0);
    let more = |hidden: usize| format!("+{hidden} more");
    if max == 1 && total > 0 {
        return vec![Line::from(vec![
            Span::styled(label("Availability"), muted),
            Span::styled(more(total), muted),
        ])];
    }
    let mut lines = vec![Line::from(vec![
        Span::styled(label("Availability"), muted),
        Span::styled(ruler(per_hour), muted),
        Span::styled(checking.to_string(), muted),
    ])];
    let picked = chosen(form, first, &answer.start, interval);
    // The time the event being edited holds now, on the shown day.
    let current = form
        .editing
        .as_ref()
        .and_then(|edit| chosen(&edit.original, first, &answer.start, interval))
        .filter(|(from, to)| *to >= 0 && *from < slots as i64);
    let own = Style::default().fg(theme::active().accent_alt);
    if current.is_some() {
        let legend = ["  \u{2588}", " this event"];
        let used: usize = lines[0]
            .spans
            .iter()
            .map(|s| s.content.chars().count())
            .sum();
        if used + legend.iter().map(|s| s.chars().count()).sum::<usize>() <= width {
            lines[0].spans.push(Span::styled(legend[0], own));
            lines[0].spans.push(Span::styled(legend[1], muted));
        }
    }
    let normal = Style::default().fg(theme::active().text);
    let marked = Style::default()
        .fg(theme::active().accent)
        .add_modifier(Modifier::UNDERLINED | Modifier::BOLD);
    for schedule in answer.schedules.iter().take(shown) {
        let mut spans = vec![Span::styled(label(&schedule.address), normal)];
        if schedule.error.is_some() {
            spans.push(Span::styled("no free/busy".to_string(), muted));
            lines.push(Line::from(spans));
            continue;
        }
        let row: Vec<char> = availability_row(&schedule.view, first, slots)
            .chars()
            .collect();
        for (at, glyph) in row.into_iter().enumerate() {
            let within = |range: Option<(i64, i64)>| {
                range.is_some_and(|(from, to)| (from..=to).contains(&(at as i64)))
            };
            let style = match (within(picked), within(current)) {
                (true, true) => marked.patch(own),
                (true, false) => marked,
                (false, true) => own,
                (false, false) => normal,
            };
            spans.push(Span::styled(glyph.to_string(), style));
        }
        lines.push(Line::from(spans));
    }
    if shown < total {
        lines.push(Line::from(Span::styled(
            format!("  {}", more(total - shown)),
            muted,
        )));
    }
    lines
}
