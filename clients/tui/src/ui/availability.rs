//! The event form's availability rows (PERSO-255): one row of glyphs per
//! person and room over the form's day from 07:00 to 20:00, one character
//! per 30-minute slot (`·` free, `▒` tentative, `█` busy, `▓` away, `░`
//! working elsewhere, `?` unknown), the chosen time's slots underlined, and
//! "no free/busy" for an address the server cannot read.

use chrono::{NaiveDateTime, NaiveTime, TimeZone, Utc};
use mp_client::format::{availability_row, slot_of};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::{Availability, EventForm};
use crate::theme;

/// The first hour a row shows.
const FIRST_HOUR: u32 = 7;
/// The hour a row ends at.
const LAST_HOUR: u32 = 20;
/// The label column's width.
const LABEL: usize = 22;
/// The most rows shown; the rest are counted.
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

/// The lines the form shows under its fields: none while the form's
/// account has no free/busy, else the header and a row per schedule.
pub(super) fn availability_lines(form: &EventForm, state: &Availability) -> Vec<Line<'static>> {
    if state.seen.is_none() {
        return Vec::new();
    }
    let muted = Style::default().fg(theme::active().text_muted);
    let mut lines = vec![Line::from("")];
    let checking = if state.checking() {
        "  checking\u{2026}"
    } else {
        ""
    };
    let answer = match &state.answer {
        None => {
            lines.push(Line::from(Span::styled(
                format!("{}{}", label("Availability"), checking.trim_start()),
                muted,
            )));
            return lines;
        }
        Some(Err(why)) => {
            lines.push(Line::from(vec![
                Span::styled(label("Availability"), muted),
                Span::styled(
                    format!("unavailable: {why}"),
                    Style::default().fg(theme::active().warning),
                ),
            ]));
            return lines;
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
        return lines;
    };
    lines.push(Line::from(vec![
        Span::styled(label("Availability"), muted),
        Span::styled(ruler(per_hour), muted),
        Span::styled(checking.to_string(), muted),
    ]));
    let picked = chosen(form, first, &answer.start, interval);
    let normal = Style::default().fg(theme::active().text);
    let marked = Style::default()
        .fg(theme::active().accent)
        .add_modifier(Modifier::UNDERLINED | Modifier::BOLD);
    for schedule in answer.schedules.iter().take(MAX_ROWS) {
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
            let on = picked.is_some_and(|(from, to)| (from..=to).contains(&(at as i64)));
            spans.push(Span::styled(
                glyph.to_string(),
                if on { marked } else { normal },
            ));
        }
        lines.push(Line::from(spans));
    }
    if answer.schedules.len() > MAX_ROWS {
        lines.push(Line::from(Span::styled(
            format!("  +{} more", answer.schedules.len() - MAX_ROWS),
            muted,
        )));
    }
    lines
}
