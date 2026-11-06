//! The event form's availability rows (PERSO-255): a timeline of the form's
//! day from 07:00 to 20:00 under its fields, one row per person, the user
//! (`You`) first, then the attendees and the rooms (`⌂`), each named by the
//! local part of its address in the form's label column, so the timeline
//! lines up with the field values above it.
//!
//! Each 30-minute slot is two cells (one on a form too narrow for two),
//! filled by its status with a glyph and a colour, so a monochrome terminal
//! still tells them apart: free is blank, `▇` busy, `▒` tentative, `▓` away,
//! `░` working elsewhere, `╱` unknown, and a row the server cannot read is
//! hatched and says "no free/busy". The hour labels sit on the hour
//! gridlines (`▏`), a legend over them names the fills, and the chosen time
//! is a band of raised background across the ruler and every row, marked
//! `━` on the line under them. An edit form draws the slots the event holds
//! now in the event's own colour, marked `┄` (`╍` where the chosen time
//! covers them too), so the organizer's busy block reads as the event
//! itself rather than a clash.
//!
//! While a query is on its way the last answer's rows stay, dimmed, and an
//! address it lacks waits as a placeholder row (`┈`); a refused query keeps
//! the rows, hatched, under its reason. The rows neither collapse nor jump
//! under the field being typed in.

use std::ops::Range;

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use mp_client::format::{availability_row, slot_of};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::util::{display_width, take_width, take_width_end, truncate};
use crate::app::{Availability, EventForm, AVAILABILITY_INTERVAL};
use crate::theme;

/// The first hour a row shows.
const FIRST_HOUR: u32 = 7;
/// The hour a row ends at.
const LAST_HOUR: u32 = 20;
/// The indent of every line, the form's own.
const INDENT: usize = 2;
/// The name column, the width of the form's label column.
const NAME: usize = 13;
/// The most rows shown however tall the terminal; the rest are counted.
const MAX_ROWS: usize = 8;

/// The glyph of a busy slot: seven eighths of the cell, so the busy
/// blocks of two rows read as two rather than one tall block.
const BUSY: char = '\u{2587}';
/// The glyph of a free cell on an hour gridline.
const GRIDLINE: char = '\u{258f}';
/// The glyph of a slot nobody knows: off the answer's view, a row the
/// server cannot read, or a refused query.
const UNKNOWN: char = '\u{2571}';
/// The glyph of a row still on its way.
const PENDING: char = '\u{2508}';
/// The glyph of a room's name.
const ROOM: &str = "\u{2302} ";

/// Who a row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    You,
    Required,
    Optional,
    Room,
}

/// What a row's timeline shows.
enum Track {
    /// The slots from 07:00, as `availability_row` glyphs.
    Slots(Vec<char>),
    /// The server cannot read this address.
    NoFreeBusy,
    /// The answer has not reached this address yet.
    Pending,
    /// The query was refused.
    Refused,
}

/// One person's row.
struct Row {
    address: String,
    kind: Kind,
    track: Track,
}

/// The timeline's geometry: `slots` slots of `cell` cells, `per_hour` an
/// hour.
struct Grid {
    slots: usize,
    per_hour: usize,
    cell: usize,
}

impl Grid {
    fn cells(&self) -> usize {
        self.slots * self.cell
    }

    /// The cells from one labelled hour gridline to the next: every hour,
    /// or every other one where an hour is too narrow for its label.
    fn step(&self) -> usize {
        let hour = self.per_hour * self.cell;
        if hour >= 3 {
            hour
        } else {
            2 * hour
        }
    }

    fn gridline(&self, cell: usize) -> bool {
        cell.is_multiple_of(self.step())
    }

    fn slot(&self, cell: usize) -> i64 {
        (cell / self.cell) as i64
    }
}

/// `text` in the name column: the indent, then the text cut to leave a
/// space before the timeline.
fn name_column(text: &str) -> String {
    let cut = truncate(text, NAME - 1);
    let pad = NAME.saturating_sub(display_width(&cut));
    format!("{:INDENT$}{cut}{:pad$}", "", "")
}

/// `text` in `width` cells: whole when it fits, else its first third, an
/// ellipsis and as much of its end as fits.
fn cut_middle(text: &str, width: usize) -> String {
    if display_width(text) <= width {
        return text.to_string();
    }
    let head = take_width(text, width / 3);
    let tail = take_width_end(text, width - display_width(&head) - 1);
    format!("{head}\u{2026}{tail}")
}

/// `text` in `width` cells around `differ`, the chars where it differs from
/// the rows it would read the same as: whole when it fits, else its middle
/// cut ending a few cells past the start of `differ` (centring `differ`
/// when it is narrower than the cells left), then an ellipsis when that
/// cuts the text short.
fn cut_around(text: &str, differ: Range<usize>, width: usize) -> String {
    let byte = |chars: usize| {
        text.char_indices()
            .nth(chars)
            .map_or(text.len(), |(at, _)| at)
    };
    let (start, end) = (byte(differ.start), byte(differ.end));
    // The cells `cut_middle` keeps of the end of a text cut to `width - 1`.
    let room = width - 1 - (width - 1) / 3 - 1;
    let part = display_width(&text[start..end]);
    let cut = start + take_width(&text[start..], room.min((room + part) / 2)).len();
    if cut == text.len() {
        return cut_middle(text, width);
    }
    format!("{}\u{2026}", cut_middle(&text[..cut], width - 1))
}

/// The chars where `text` differs from `others`: from the first char not
/// all of them share at its start to the last not all share at its end.
fn differ(text: &str, others: &[&str]) -> Range<usize> {
    fn shared(a: impl Iterator<Item = char>, b: impl Iterator<Item = char>) -> usize {
        a.zip(b).take_while(|(a, b)| a == b).count()
    }
    let len = text.chars().count();
    let start = others
        .iter()
        .map(|other| shared(text.chars(), other.chars()))
        .min()
        .unwrap_or(len);
    let end = others
        .iter()
        .map(|other| len - shared(text.chars().rev(), other.chars().rev()))
        .max()
        .unwrap_or(start);
    start..end.max(start)
}

/// Each row's name column: "You" for the user, the address's local part
/// for the others, after the glyph for a room.
///
/// Rows whose names read the same name themselves by a longer text
/// instead: the whole address when another row has the same local part
/// (info@a.com and info@b.com), else the local part. Each such row takes
/// the first of three forms of that text that reads unlike every other
/// row's name: cut at its end (`office@ls1.…`), cut in the middle to its
/// start and as much of its end as fits (`info…ny-a.de`), or cut around
/// where it differs from the rows it still reads the same as
/// (`con…pt-1.ex…`). No two rows read the same unless they do in every
/// form; the third shows the first char where a row differs from the rest
/// of its group, so this takes three or more rows that differ from each
/// other at places too far apart for one name, or two that differ only
/// in chars the terminal draws alike.
fn row_names(rows: &[Row]) -> Vec<String> {
    fn local(row: &Row) -> &str {
        row.address.split('@').next().unwrap_or_default()
    }
    let glyph = |row: &Row| if row.kind == Kind::Room { ROOM } else { "" };
    let column = |row: &Row, text: &str| name_column(&format!("{}{text}", glyph(row)));
    let width = |row: &Row| NAME - 1 - display_width(glyph(row));
    let texts: Vec<&str> = rows
        .iter()
        .map(|row| {
            let shared = rows
                .iter()
                .filter(|other| local(other) == local(row))
                .count()
                > 1;
            if shared {
                row.address.as_str()
            } else {
                local(row)
            }
        })
        .collect();
    let mut names: Vec<Option<String>> = rows
        .iter()
        .map(|row| (row.kind == Kind::You).then(|| name_column("You")))
        .collect();
    let mut forms: Vec<String> = rows.iter().map(|row| column(row, local(row))).collect();
    for form in 0..4 {
        let open: Vec<usize> = (0..rows.len()).filter(|&i| names[i].is_none()).collect();
        if form > 0 {
            let before = forms.clone();
            for &i in &open {
                let (row, text) = (&rows[i], texts[i]);
                let cut = match form {
                    1 => text.to_string(),
                    2 => cut_middle(text, width(row)),
                    _ => {
                        let others: Vec<&str> = open
                            .iter()
                            .filter(|&&j| j != i && before[j] == before[i])
                            .map(|&j| texts[j])
                            .collect();
                        cut_around(text, differ(text, &others), width(row))
                    }
                };
                forms[i] = column(row, &cut);
            }
        }
        let unique: Vec<usize> = open
            .iter()
            .copied()
            .filter(|&i| {
                open.iter().filter(|&&j| forms[j] == forms[i]).count() == 1
                    && !names.contains(&Some(forms[i].clone()))
            })
            .collect();
        for i in unique {
            names[i] = Some(forms[i].clone());
        }
    }
    names
        .into_iter()
        .zip(forms)
        .map(|(name, form)| name.unwrap_or(form))
        .collect()
}

/// Push `glyph` in `style`, extending the last span when it has the same
/// style, so a row is a handful of spans rather than one per cell.
fn push(spans: &mut Vec<Span<'static>>, glyph: char, style: Style) {
    if let Some(last) = spans.last_mut() {
        if last.style == style {
            last.content.to_mut().push(glyph);
            return;
        }
    }
    spans.push(Span::styled(glyph.to_string(), style));
}

/// The slots of `form`'s time, as offsets from the slot that starts at
/// `origin`, on a timed event.
fn chosen(form: &EventForm, origin: DateTime<Utc>, interval: u32) -> Option<(i64, i64)> {
    if form.all_day {
        return None;
    }
    let zone = mp_core::calendar_model::parse_zone(&form.zone)?;
    let date = NaiveDate::parse_from_str(form.date.trim(), "%Y-%m-%d").ok()?;
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
    let seconds = i64::from(interval.max(1)) * 60;
    let first = (from - origin).num_seconds().div_euclid(seconds);
    let last = ((to - origin).num_seconds() - 1).div_euclid(seconds);
    Some((first, last))
}

/// Whether slot `slot` lies in `range`.
fn within(range: Option<(i64, i64)>, slot: i64) -> bool {
    range.is_some_and(|(from, to)| (from..=to).contains(&slot))
}

/// The lines the form shows under its fields, at most `max` of them in
/// `width` columns: none while the form's account has no free/busy, else
/// the legend (or the refusal), the hour ruler, a row per person and the
/// line marking the chosen time. A short terminal gives up the legend, then
/// the marks, then rows, counting the rows that do not fit, so the form
/// keeps its problem line; one line alone is the count, or the refusal.
pub(super) fn availability_lines(
    form: &EventForm,
    state: &Availability,
    width: usize,
    max: usize,
) -> Vec<Line<'static>> {
    let Some((seen, _)) = &state.seen else {
        return Vec::new();
    };
    if max == 0 {
        return Vec::new();
    }
    let Some(zone) = mp_core::calendar_model::parse_zone(&form.zone) else {
        return Vec::new();
    };
    let theme = theme::active();
    let muted = Style::default().fg(theme.text_muted);
    let faint = Style::default().fg(theme.text_faint);
    // Busy and tentative take the desktop's Pumpkin, which the `code` slot
    // carries in the app's palettes and holds on every ground; `selection`
    // is White in the terminal theme, made for the DarkGray cursor row, and
    // would vanish on a light terminal.
    let busy = theme.code;
    let (answer, refusal) = match &state.answer {
        Some(Ok(answer)) => (Some(answer), None),
        Some(Err(why)) => (None, Some(why.as_str())),
        None => (None, None),
    };
    // The answer on screen is for another query than the form's while one
    // is on its way or the form changed since: its rows stay, dimmed.
    let stale = state.answer.is_some() && (state.checking() || state.asked.as_ref() != Some(seen));
    let checking = stale || state.answer.is_none();
    let dim = if stale {
        Modifier::DIM
    } else {
        Modifier::empty()
    };

    let interval = answer.map_or(AVAILABILITY_INTERVAL, |a| a.interval_minutes.max(1));
    let per_hour = (60 / interval).max(1) as usize;
    let slots = (LAST_HOUR - FIRST_HOUR) as usize * per_hour;
    let cell = if width >= INDENT + NAME + 2 * slots {
        2
    } else {
        1
    };
    let grid = Grid {
        slots,
        per_hour,
        cell,
    };
    // The day the rows read: the answer's, which the form may have moved
    // off while the next query runs, else the form's own.
    let day = answer
        .and_then(|a| DateTime::parse_from_rfc3339(&a.start).ok())
        .map(|start| start.with_timezone(&zone).date_naive())
        .or_else(|| NaiveDate::parse_from_str(form.date.trim(), "%Y-%m-%d").ok());
    let Some(origin) = day
        .and_then(|day| {
            zone.from_local_datetime(&day.and_hms_opt(FIRST_HOUR, 0, 0)?)
                .earliest()
        })
        .map(|at| at.with_timezone(&Utc))
    else {
        return Vec::new();
    };
    let first = match answer {
        Some(answer) => match slot_of(&answer.start, interval, origin) {
            Some(first) => first,
            None => return Vec::new(),
        },
        None => 0,
    };

    // The rows: the answer's (the user's own first), then the form's
    // addresses it has not reached; before an answer the user's row waits
    // too.
    let listed = |list: &str, address: &str| {
        list.split([',', ';'])
            .map(str::trim)
            .any(|a| a.eq_ignore_ascii_case(address))
    };
    let kind_of = |address: &str| {
        if listed(&form.rooms, address) {
            Kind::Room
        } else if listed(&form.optional, address) {
            Kind::Optional
        } else {
            Kind::Required
        }
    };
    let waiting = || {
        if refusal.is_some() {
            Track::Refused
        } else {
            Track::Pending
        }
    };
    let mut rows: Vec<Row> = match answer {
        Some(answer) => answer
            .schedules
            .iter()
            .enumerate()
            .map(|(n, schedule)| Row {
                address: schedule.address.clone(),
                kind: if n == 0 {
                    Kind::You
                } else {
                    kind_of(&schedule.address)
                },
                track: if schedule.error.is_some() {
                    Track::NoFreeBusy
                } else {
                    Track::Slots(
                        availability_row(&schedule.view, first, slots)
                            .chars()
                            .collect(),
                    )
                },
            })
            .collect(),
        None => vec![Row {
            address: String::new(),
            kind: Kind::You,
            track: waiting(),
        }],
    };
    for address in &seen.addresses {
        if !rows.iter().any(|r| r.address.eq_ignore_ascii_case(address)) {
            rows.push(Row {
                address: address.clone(),
                kind: kind_of(address),
                track: waiting(),
            });
        }
    }

    // The budget: the ruler (and a refusal's line), as many rows as fit
    // with the marks line counting the rest, then the marks line and the
    // legend while room is left.
    let total = rows.len();
    let base = 1 + usize::from(refusal.is_some());
    let fits = |shown: usize| base + shown + usize::from(shown < total) <= max;
    // The "Availability" line beside `text`, the refusal cut to the width.
    let titled = |text: String, style: Style| {
        Line::from(vec![
            Span::styled(name_column("Availability"), muted),
            Span::styled(truncate(&text, width.saturating_sub(INDENT + NAME)), style),
        ])
    };
    let refused = |why: &str| {
        titled(
            format!("unavailable: {why}"),
            Style::default().fg(theme.warning).add_modifier(dim),
        )
    };
    let Some(shown) = (0..=total.min(MAX_ROWS)).rev().find(|rows| fits(*rows)) else {
        return vec![match refusal {
            Some(why) => refused(why),
            None if answer.is_none() => titled("checking\u{2026}".into(), muted),
            None => titled(format!("+{total} more"), muted),
        }];
    };
    let mut left = max - (base + shown + usize::from(shown < total));
    let marks = shown < total || left > 0;
    if shown == total && marks {
        left -= 1;
    }
    let legend = refusal.is_none() && left > 0;

    let picked = chosen(form, origin, interval);
    // The time the event being edited holds now, on the shown day.
    let current = form
        .editing
        .as_ref()
        .and_then(|edit| chosen(&edit.original, origin, interval))
        .filter(|(from, to)| *to >= 0 && *from < slots as i64);
    let band = |style: Style, slot: i64| {
        if within(picked, slot) {
            style.bg(theme.surface)
        } else {
            style
        }
    };

    let mut lines = Vec::with_capacity(shown + 3);
    if let Some(why) = refusal {
        lines.push(refused(why));
    } else if legend {
        let mut spans = vec![Span::styled(name_column("Availability"), muted)];
        let mut used = INDENT + NAME;
        let mut items = vec![
            (BUSY, "busy", busy),
            ('\u{2592}', "tentative", busy),
            ('\u{2593}', "away", theme.error),
            ('\u{2591}', "elsewhere", theme.accent),
        ];
        if current.is_some() {
            items.push(('\u{2504}', "this event", theme.accent_alt));
        }
        for (n, (glyph, word, colour)) in items.into_iter().enumerate() {
            let gap = if n == 0 { "" } else { "  " };
            let wide = gap.len() + 2 + word.len();
            if used + wide > width {
                break;
            }
            used += wide;
            spans.push(Span::raw(gap));
            spans.push(Span::styled(glyph.to_string(), Style::default().fg(colour)));
            spans.push(Span::styled(format!(" {word}"), muted));
        }
        lines.push(Line::from(spans));
    }

    // The ruler: an hour's label on each gridline, under the legend's
    // "Availability" or, without the legend, beside its own.
    let ruler_label = if checking {
        "checking\u{2026}"
    } else if legend || refusal.is_some() {
        ""
    } else {
        "Availability"
    };
    let mut spans = vec![Span::styled(name_column(ruler_label), muted)];
    let mut labels: Vec<char> = vec![' '; grid.cells()];
    for at in (0..grid.cells()).filter(|at| grid.gridline(*at)) {
        let hour = FIRST_HOUR as usize + at / (grid.per_hour * grid.cell);
        for (n, digit) in format!("{hour:02}").chars().enumerate() {
            if let Some(cell) = labels.get_mut(at + n) {
                *cell = digit;
            }
        }
    }
    for (at, glyph) in labels.into_iter().enumerate() {
        push(&mut spans, glyph, band(muted, grid.slot(at)));
    }
    lines.push(Line::from(spans));

    let normal = Style::default().fg(theme.text);
    let names = row_names(&rows);
    for (row, full) in rows.iter().zip(names).take(shown) {
        let mut spans = match row.kind {
            Kind::You => vec![Span::styled(full, normal.add_modifier(Modifier::BOLD))],
            kind => {
                let style = if kind == Kind::Optional {
                    muted
                } else {
                    normal
                };
                if kind == Kind::Room {
                    // The room's glyph muted, its name cut after it.
                    let split = full
                        .char_indices()
                        .nth(INDENT + ROOM.chars().count())
                        .map_or(full.len(), |(at, _)| at);
                    vec![
                        Span::styled(full[..split].to_string(), muted),
                        Span::styled(full[split..].to_string(), style),
                    ]
                } else {
                    vec![Span::styled(full, style)]
                }
            }
        };
        let cells: Vec<(char, Style)> = match &row.track {
            Track::Slots(glyphs) => (0..grid.cells())
                .map(|at| {
                    let slot = grid.slot(at);
                    let (glyph, colour) = match glyphs.get(slot as usize) {
                        Some('\u{b7}') if grid.gridline(at) => (GRIDLINE, theme.text_faint),
                        Some('\u{b7}') => (' ', theme.text_faint),
                        Some('\u{2588}') => (BUSY, busy),
                        Some('\u{2592}') => ('\u{2592}', busy),
                        Some('\u{2593}') => ('\u{2593}', theme.error),
                        Some('\u{2591}') => ('\u{2591}', theme.accent),
                        _ => (UNKNOWN, theme.text_faint),
                    };
                    let colour = if within(current, slot) {
                        theme.accent_alt
                    } else {
                        colour
                    };
                    (glyph, Style::default().fg(colour))
                })
                .collect(),
            Track::NoFreeBusy => {
                let words = " no free/busy";
                let hatch = grid.cells().saturating_sub(words.len());
                (0..hatch)
                    .map(|_| (UNKNOWN, faint))
                    .chain(words.chars().map(|c| (c, muted)))
                    .take(grid.cells())
                    .collect()
            }
            Track::Refused => (0..grid.cells()).map(|_| (UNKNOWN, faint)).collect(),
            Track::Pending => (0..grid.cells()).map(|_| (PENDING, faint)).collect(),
        };
        for (at, (glyph, style)) in cells.into_iter().enumerate() {
            push(
                &mut spans,
                glyph,
                band(style, grid.slot(at)).add_modifier(dim),
            );
        }
        lines.push(Line::from(spans));
    }

    if marks {
        let more = if shown < total {
            format!("+{} more", total - shown)
        } else {
            String::new()
        };
        let mut spans = vec![Span::styled(name_column(&more), muted)];
        let accent = Style::default().fg(theme.accent);
        let own = Style::default().fg(theme.accent_alt);
        for at in 0..grid.cells() {
            let slot = grid.slot(at);
            let (glyph, style) = match (within(picked, slot), within(current, slot)) {
                (true, true) => ('\u{254d}', accent),
                (true, false) => ('\u{2501}', accent),
                (false, true) => ('\u{2504}', own),
                (false, false) => (' ', Style::default()),
            };
            push(&mut spans, glyph, style);
        }
        lines.push(Line::from(spans));
    }
    lines
}
