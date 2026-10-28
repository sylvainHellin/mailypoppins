//! What `mp calendar …` and `mp event …` print.
//!
//! The fold behind `mp calendar rebuild` is `calendar.rebuild`'s since P4-U14
//! and its direct handler went with the rest of the direct engine paths in
//! P4-U15; its four literals are printed here from whatever the daemon
//! reports, with the server resync's report beside them (PERSO-200).
//!
//! The server calendar's views (PERSO-200) are pure functions over the wire
//! types of `mp_protocol::calendar` and the viewer's zone, so the whole layout
//! is tested without a daemon: which days a range covers ([`view_days`]), the
//! `calendar.view` parameters for them ([`view_params`]), the agenda, day and
//! week text ([`render_view`]), the calendar list ([`render_calendars`]) and
//! the event card ([`render_event`]).

use std::collections::HashMap;

use chrono::{DateTime, Datelike, Days, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use colored::*;
use mp_protocol::calendar::{CalendarEvent, CalendarEventTime, CalendarInfo, CalendarViewParams};
use serde_json::Value;

/// The line printed before one account's fold.
pub fn print_header(account: &str) {
    println!(
        "{} Reconciling calendar replies for {} …",
        "ℹ".blue(),
        account.yellow()
    );
}

/// What an account with no store gets: a note, and the walk carries on.
pub fn print_no_store(account: &str) {
    println!("{} no store yet for {}", "•".blue(), account);
}

/// What the fold resolved, and the cancellations it saw.
pub fn print_report(resolved: usize, invites_seen: usize, replies_seen: usize, cancelled: usize) {
    {
        let report = ReportCounts {
            resolved,
            invites_seen,
            replies_seen,
            cancelled,
        };
        println!(
            "{} {} attendee status(es) resolved across {} invite(s) / {} reply(ies)",
            "✓".green(),
            report.resolved.to_string().bold(),
            report.invites_seen,
            report.replies_seen,
        );
        if report.cancelled > 0 {
            println!(
                "{} {} invite(s) cancelled by the organizer (kept, marked cancelled)",
                "•".blue(),
                report.cancelled.to_string().bold(),
            );
        }
    }
}

/// The four counts one fold reports, named so the block above reads as it did.
struct ReportCounts {
    resolved: usize,
    invites_seen: usize,
    replies_seen: usize,
    cancelled: usize,
}

/// What `calendar.rebuild`'s resync of the server cache did, the `server`
/// member of its settled result: the counts, then each calendar that failed,
/// or the error that stopped the resync.
pub fn print_server_report(server: &Value) {
    for line in server_report_lines(server) {
        println!("{line}");
    }
}

/// [`print_server_report`]'s lines, glyphs coloured.
fn server_report_lines(server: &Value) -> Vec<String> {
    if let Some(error) = server.get("error") {
        return vec![format!(
            "{} server calendar not synced: {}",
            "✗".red(),
            error["message"].as_str().unwrap_or("unknown error")
        )];
    }
    let count = |key: &str| server[key].as_u64().unwrap_or_default();
    let mut lines = vec![format!(
        "{} server calendar: {} calendar(s), {} event(s) synced, {} removed, {} baseline(s)",
        "✓".green(),
        count("calendars").to_string().bold(),
        count("upserts"),
        count("removed"),
        count("baselines"),
    )];
    for failed in server["failed"].as_array().into_iter().flatten() {
        lines.push(format!(
            "  {} {}: {}",
            "✗".red(),
            failed["calendar"].as_str().unwrap_or_default(),
            failed["error"].as_str().unwrap_or_default()
        ));
    }
    lines
}

// ---------------------------------------------------------------------------
// Dates and ranges
// ---------------------------------------------------------------------------

/// The forms [`parse_date`] accepts, as one line for an error.
pub const DATE_HELP: &str =
    "YYYY-MM-DD, today, tomorrow, yesterday, or +N / -N for N days from today";

/// How many days an agenda shows after its first one by default.
pub const AGENDA_DAYS: u64 = 14;

/// A day a calendar command names, read against `today`.
///
/// A small closed grammar, forward-looking where the TUI's jump-to-date
/// (`mp_tui::app::jump_date`) looks back through an archive: a view is about
/// what comes next. `Err` carries a one-line reason.
pub fn parse_date(input: &str, today: NaiveDate) -> Result<NaiveDate, String> {
    let raw = input.trim();
    if let Ok(date) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return Ok(date);
    }
    let shifted = |days: i64| {
        let moved = if days >= 0 {
            today.checked_add_days(Days::new(days.unsigned_abs()))
        } else {
            today.checked_sub_days(Days::new(days.unsigned_abs()))
        };
        moved.ok_or_else(|| format!("{raw} is out of range"))
    };
    match raw.to_ascii_lowercase().as_str() {
        "today" => return Ok(today),
        "tomorrow" => return shifted(1),
        "yesterday" => return shifted(-1),
        _ => {}
    }
    if let Some(sign) = raw.chars().next().filter(|c| *c == '+' || *c == '-') {
        if let Ok(n) = raw[1..].parse::<i64>() {
            return shifted(if sign == '-' { -n } else { n });
        }
    }
    Err(format!("cannot read {raw:?} as a date; use {DATE_HELP}"))
}

/// The three layouts of `mp calendar view`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// Day by day, only the days with events.
    Agenda,
    /// One day.
    Day,
    /// The ISO week, Monday to Sunday, every day listed.
    Week,
}

/// The first and the last day, both included, that `layout` shows for the
/// dates the command named: `on` the day it is about (today when absent),
/// `from` and `to` an agenda's own range.
pub fn view_days(
    layout: Layout,
    on: Option<NaiveDate>,
    range: Option<(NaiveDate, NaiveDate)>,
    today: NaiveDate,
) -> Result<(NaiveDate, NaiveDate), String> {
    let day = on.unwrap_or(today);
    match (layout, range) {
        (Layout::Agenda, Some((from, to))) if to < from => {
            Err(format!("--to {to} is before --from {from}"))
        }
        (Layout::Agenda, Some(range)) => Ok(range),
        (Layout::Agenda, None) => Ok((day, day + Days::new(AGENDA_DAYS))),
        (Layout::Day | Layout::Week, Some(_)) => {
            Err("--from and --to set an agenda's range; a day or a week takes --on".into())
        }
        (Layout::Day, None) => Ok((day, day)),
        (Layout::Week, None) => {
            let monday = day - Days::new(u64::from(day.weekday().num_days_from_monday()));
            Ok((monday, monday + Days::new(6)))
        }
    }
}

/// The instant `day` starts in `zone`: its midnight, or the first instant
/// after it when a daylight-saving jump skips midnight there.
pub fn day_start(zone: Tz, day: NaiveDate) -> DateTime<Utc> {
    let midnight = day.and_time(chrono::NaiveTime::MIN);
    (0..=3)
        .find_map(|hour| {
            zone.from_local_datetime(&(midnight + chrono::Duration::hours(hour)))
                .earliest()
        })
        .map(|instant| instant.with_timezone(&Utc))
        .unwrap_or_else(|| midnight.and_utc())
}

/// The `calendar.view` parameters of the days `first` to `last` in `zone`.
pub fn view_params(
    (first, last): (NaiveDate, NaiveDate),
    zone: Tz,
    accounts: Option<Vec<String>>,
    calendars: Option<Vec<String>>,
    include_hidden: bool,
) -> CalendarViewParams {
    let start = day_start(zone, first).with_timezone(&zone);
    let end = day_start(zone, last + Days::new(1)).with_timezone(&zone);
    CalendarViewParams {
        start: start.to_rfc3339(),
        end: end.to_rfc3339(),
        accounts,
        calendars,
        include_hidden: include_hidden.then_some(true),
        zone: Some(zone.name().to_string()),
    }
}

// ---------------------------------------------------------------------------
// The views
// ---------------------------------------------------------------------------

/// The calendars' names by account and id, for the tag an event line
/// carries.
pub type CalendarNames = HashMap<(String, String), String>;

/// [`CalendarNames`] from a `calendar.list` answer.
pub fn calendar_names(calendars: &[CalendarInfo]) -> CalendarNames {
    calendars
        .iter()
        .map(|c| ((c.account.clone(), c.id.clone()), c.name.clone()))
        .collect()
}

/// The `account/calendar` tag of an event, the calendar by name when the
/// list names it.
fn calendar_tag(event: &CalendarEvent, names: &CalendarNames) -> String {
    let name = names
        .get(&(event.account.clone(), event.calendar.clone()))
        .map(String::as_str)
        .unwrap_or(&event.calendar);
    format!("{}/{}", event.account, name)
}

/// The one-character mark of the user's own answer.
pub fn response_mark(own_response: &str) -> char {
    match own_response {
        "accepted" => '✓',
        "tentative" => '~',
        "declined" => '✗',
        "needs-action" => '?',
        "organizer" => '★',
        _ => ' ',
    }
}

fn instant(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|instant| instant.with_timezone(&Utc))
}

fn date(raw: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok()
}

/// The time column of `event` on `day` in `zone`, or `None` when the event
/// does not fall on that day: `all day`, or the part of the day it covers,
/// `24:00` where it runs on past midnight.
fn time_on(event: &CalendarEvent, day: NaiveDate, zone: Tz) -> Option<String> {
    match &event.time {
        CalendarEventTime::AllDay { start, end } => {
            let (start, end) = (date(start)?, date(end)?);
            let covers = if end > start {
                start <= day && day < end
            } else {
                day == start
            };
            covers.then(|| "all day".to_string())
        }
        CalendarEventTime::Timed { start, end, .. } => {
            let (start, end) = (instant(start)?, instant(end)?);
            let (open, close) = (day_start(zone, day), day_start(zone, day + Days::new(1)));
            let clock = |at: DateTime<Utc>| at.with_timezone(&zone).format("%H:%M").to_string();
            if end <= start {
                return (open <= start && start < close).then(|| clock(start));
            }
            if start >= close || end <= open {
                return None;
            }
            let until = if end >= close {
                "24:00".to_string()
            } else {
                clock(end)
            };
            Some(format!("{}-{until}", clock(start.max(open))))
        }
    }
}

/// One event's line under its day.
fn event_line(event: &CalendarEvent, time: &str, names: &CalendarNames) -> String {
    let mut line = format!(
        "  {time:<11}  {} {}",
        response_mark(&event.own_response),
        if event.summary.is_empty() {
            "(no title)"
        } else {
            &event.summary
        }
    );
    if event.cancelled {
        line.push_str(" (cancelled)");
    }
    if let Some(location) = event.location.as_deref().filter(|l| !l.trim().is_empty()) {
        line.push_str(&format!("  @ {location}"));
    }
    line.push_str(&format!("  [{}]", calendar_tag(event, names)));
    line
}

/// A day's heading: `Mon 2026-07-06`, marked when it is today.
fn day_heading(day: NaiveDate, today: NaiveDate) -> String {
    let mut heading = day.format("%a %Y-%m-%d").to_string();
    if day == today {
        heading.push_str("  (today)");
    }
    heading
}

/// `mp calendar view`: the events of `days` in `zone`, laid out as `layout`.
///
/// The events come in `calendar.view`'s order, by start with an all-day
/// event first on its day, and keep it under each day they fall on; an event
/// over several days is listed under each of them.
pub fn render_view(
    layout: Layout,
    (first, last): (NaiveDate, NaiveDate),
    today: NaiveDate,
    zone: Tz,
    events: &[CalendarEvent],
    names: &CalendarNames,
) -> String {
    let mut out = String::new();
    if layout == Layout::Week {
        out.push_str(&format!(
            "Week {}, {first} to {last} ({})\n",
            first.iso_week().week(),
            zone.name()
        ));
    }
    let mut any = false;
    for day in first.iter_days().take_while(|day| *day <= last) {
        let lines: Vec<String> = events
            .iter()
            .filter_map(|event| {
                time_on(event, day, zone).map(|time| event_line(event, &time, names))
            })
            .collect();
        if lines.is_empty() && layout == Layout::Agenda {
            continue;
        }
        if any || layout == Layout::Week {
            out.push('\n');
        }
        any = true;
        out.push_str(&day_heading(day, today));
        out.push('\n');
        if lines.is_empty() {
            out.push_str("  no events\n");
        }
        for line in lines {
            out.push_str(&line);
            out.push('\n');
        }
    }
    if !any {
        out.push_str(&format!("No events from {first} to {last}\n"));
    }
    out
}

/// `mp calendar list`: one line per calendar, aligned, in `calendar.list`'s
/// order.
pub fn render_calendars(calendars: &[CalendarInfo]) -> String {
    if calendars.is_empty() {
        return "No calendars\n".to_string();
    }
    let width = |field: fn(&CalendarInfo) -> &str| {
        calendars
            .iter()
            .map(|c| field(c).chars().count())
            .max()
            .unwrap_or(0)
    };
    let (account, name, id, backend) = (
        width(|c| &c.account),
        width(|c| &c.name),
        width(|c| &c.id),
        width(|c| &c.backend),
    );
    let mut out = String::new();
    for c in calendars {
        let line = format!(
            "{:account$}  {:name$}  {:id$}  {:backend$}  {:7}  {}{}",
            c.account,
            c.name,
            c.id,
            c.backend,
            if c.visible { "visible" } else { "hidden" },
            c.color.as_deref().unwrap_or("-"),
            if c.is_default { "  (default)" } else { "" },
        );
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// When an event happens, read in `zone`: `Thu 2026-07-09 11:00-12:00`, or
/// its two ends when it spans days, or its dates when it is all-day.
fn when(event: &CalendarEvent, zone: Tz) -> String {
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
        CalendarEventTime::Timed {
            start,
            end,
            zone: own,
        } => {
            let (Some(start), Some(end)) = (instant(start), instant(end)) else {
                return format!("{start} to {end}");
            };
            let (start, end) = (start.with_timezone(&zone), end.with_timezone(&zone));
            let mut text = if start.date_naive() == end.date_naive() {
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
            };
            text.push_str(&format!(" ({})", zone.name()));
            if !own.is_empty() && own != zone.name() && own != "UTC" {
                text.push_str(&format!(", set in {own}"));
            }
            text
        }
    }
}

fn person(name: Option<&str>, email: &str) -> String {
    match name.filter(|name| !name.trim().is_empty()) {
        Some(name) => format!("{name} <{email}>"),
        None => email.to_string(),
    }
}

/// `mp event show`: one event as a card, its times read in `zone`.
pub fn render_event(event: &CalendarEvent, zone: Tz, names: &CalendarNames) -> String {
    let mut out = String::new();
    let title = if event.summary.is_empty() {
        "(no title)"
    } else {
        &event.summary
    };
    out.push_str(title);
    if event.cancelled {
        out.push_str(" (cancelled)");
    }
    out.push('\n');
    let mut field = |label: &str, value: &str| {
        out.push_str(&format!("  {label:<11} {value}\n"));
    };
    field("when", &when(event, zone));
    if let Some(location) = event.location.as_deref().filter(|l| !l.trim().is_empty()) {
        field("where", location);
    }
    field("calendar", &calendar_tag(event, names));
    if let Some(organizer) = &event.organizer {
        field(
            "organizer",
            &person(organizer.name.as_deref(), &organizer.email),
        );
    }
    for (n, attendee) in event.attendees.iter().enumerate() {
        let line = format!(
            "{}  {}, {}",
            person(attendee.name.as_deref(), &attendee.email),
            attendee.role,
            attendee.status
        );
        field(if n == 0 { "attendees" } else { "" }, &line);
    }
    field(
        "response",
        &format!(
            "{} {}",
            response_mark(&event.own_response),
            event.own_response
        ),
    );
    if let Some(recurrence) = event.recurrence.as_deref() {
        field("recurrence", recurrence);
    } else if event.series_master_id.is_some() {
        field("recurrence", "one occurrence of a series");
    }
    if let Some(url) = event.online_meeting_url.as_deref() {
        field("online", url);
    }
    if let Some(link) = event.web_link.as_deref() {
        field("link", link);
    }
    if let Some(source) = &event.source {
        field("mail", &source.selector);
    }
    field("id", &event.id);
    if let Some(body) = event
        .body
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
    {
        out.push('\n');
        out.push_str(body);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp_protocol::calendar::{CalendarAttendee, CalendarPerson};

    fn d(raw: &str) -> NaiveDate {
        date(raw).unwrap()
    }

    fn today() -> NaiveDate {
        d("2026-07-08")
    }

    fn berlin() -> Tz {
        "Europe/Berlin".parse().unwrap()
    }

    fn event(id: &str, summary: &str, time: CalendarEventTime) -> CalendarEvent {
        CalendarEvent {
            account: "evoqs".into(),
            calendar: "cal-main".into(),
            id: id.into(),
            uid: format!("{id}@example.com"),
            recurrence_id: None,
            series_master_id: None,
            summary: summary.into(),
            time,
            location: None,
            body: None,
            organizer: None,
            attendees: Vec::new(),
            own_response: "none".into(),
            recurrence: None,
            reminder: None,
            online_meeting_url: None,
            sensitivity: "normal".into(),
            show_as: "busy".into(),
            cancelled: false,
            is_organizer: false,
            last_modified: None,
            web_link: None,
            source: None,
        }
    }

    fn timed(start: &str, end: &str) -> CalendarEventTime {
        CalendarEventTime::Timed {
            start: start.into(),
            end: end.into(),
            zone: "Europe/Berlin".into(),
        }
    }

    fn all_day(start: &str, end: &str) -> CalendarEventTime {
        CalendarEventTime::AllDay {
            start: start.into(),
            end: end.into(),
        }
    }

    fn names() -> CalendarNames {
        calendar_names(&[CalendarInfo {
            account: "evoqs".into(),
            id: "cal-main".into(),
            name: "Calendar".into(),
            color: None,
            is_default: true,
            can_edit: true,
            visible: true,
            backend: "graph".into(),
        }])
    }

    #[test]
    fn dates_read_iso_words_and_offsets() {
        let parse = |raw: &str| parse_date(raw, today());
        assert_eq!(parse("2026-12-24"), Ok(d("2026-12-24")));
        assert_eq!(parse("today"), Ok(today()));
        assert_eq!(parse("Tomorrow"), Ok(d("2026-07-09")));
        assert_eq!(parse("yesterday"), Ok(d("2026-07-07")));
        assert_eq!(parse("+30"), Ok(d("2026-08-07")));
        assert_eq!(parse("-8"), Ok(d("2026-06-30")));
        for bad in ["next week", "2026-13-01", "+", "+x", ""] {
            let err = parse(bad).unwrap_err();
            assert!(err.contains("YYYY-MM-DD"), "{bad}: {err}");
        }
    }

    #[test]
    fn each_layout_covers_its_days() {
        let days = |layout, on, range| view_days(layout, on, range, today());
        assert_eq!(
            days(Layout::Agenda, None, None),
            Ok((today(), d("2026-07-22")))
        );
        assert_eq!(
            days(Layout::Agenda, Some(d("2026-08-01")), None),
            Ok((d("2026-08-01"), d("2026-08-15")))
        );
        assert_eq!(
            days(
                Layout::Agenda,
                None,
                Some((d("2026-08-01"), d("2026-08-31")))
            ),
            Ok((d("2026-08-01"), d("2026-08-31")))
        );
        assert!(days(
            Layout::Agenda,
            None,
            Some((d("2026-08-31"), d("2026-08-01")))
        )
        .is_err());
        assert_eq!(days(Layout::Day, None, None), Ok((today(), today())));
        // 2026-07-08 is a Wednesday: its ISO week runs Monday 6 to Sunday 12.
        assert_eq!(
            days(Layout::Week, None, None),
            Ok((d("2026-07-06"), d("2026-07-12")))
        );
        assert_eq!(
            days(Layout::Week, Some(d("2026-07-12")), None),
            Ok((d("2026-07-06"), d("2026-07-12")))
        );
        assert!(days(Layout::Week, None, Some((today(), today()))).is_err());
    }

    #[test]
    fn the_view_runs_from_midnight_to_midnight_in_the_viewers_zone() {
        let params = view_params(
            (d("2026-07-06"), d("2026-07-12")),
            berlin(),
            Some(vec!["evoqs".into()]),
            None,
            false,
        );
        assert_eq!(params.start, "2026-07-06T00:00:00+02:00");
        assert_eq!(params.end, "2026-07-13T00:00:00+02:00");
        assert_eq!(params.zone.as_deref(), Some("Europe/Berlin"));
        assert_eq!(params.include_hidden, None);
        // Midnight skipped by a daylight-saving jump: the first hour there is.
        let havana: Tz = "America/Havana".parse().unwrap();
        assert_eq!(
            day_start(havana, d("2026-03-08")).to_rfc3339(),
            "2026-03-08T05:00:00+00:00"
        );
    }

    #[test]
    fn an_agenda_lists_each_event_under_every_day_it_covers() {
        let mut meeting = event(
            "ev-1",
            "Project review",
            timed("2026-07-08T07:00:00Z", "2026-07-08T08:00:00Z"),
        );
        meeting.location = Some("Room 4.12".into());
        meeting.own_response = "accepted".into();
        let holiday = event("ev-2", "Holiday", all_day("2026-07-10", "2026-07-12"));
        let mut late = event(
            "ev-3",
            "Night shift",
            timed("2026-07-10T20:00:00Z", "2026-07-10T23:30:00Z"),
        );
        late.account = "perso".into();
        late.calendar = "imip".into();
        late.own_response = "needs-action".into();
        let text = render_view(
            Layout::Agenda,
            (today(), d("2026-07-22")),
            today(),
            berlin(),
            &[meeting, holiday, late],
            &names(),
        );
        assert_eq!(
            text,
            "Wed 2026-07-08  (today)\n\
             \x20 09:00-10:00  ✓ Project review  @ Room 4.12  [evoqs/Calendar]\n\
             \n\
             Fri 2026-07-10\n\
             \x20 all day        Holiday  [evoqs/Calendar]\n\
             \x20 22:00-24:00  ? Night shift  [perso/imip]\n\
             \n\
             Sat 2026-07-11\n\
             \x20 all day        Holiday  [evoqs/Calendar]\n\
             \x20 00:00-01:30  ? Night shift  [perso/imip]\n"
        );
    }

    #[test]
    fn a_week_lists_every_day_and_a_day_says_when_it_is_empty() {
        let meeting = event(
            "ev-1",
            "Standup",
            timed("2026-07-07T07:00:00Z", "2026-07-07T07:15:00Z"),
        );
        let week = render_view(
            Layout::Week,
            (d("2026-07-06"), d("2026-07-12")),
            today(),
            berlin(),
            std::slice::from_ref(&meeting),
            &names(),
        );
        assert!(
            week.starts_with("Week 28, 2026-07-06 to 2026-07-12 (Europe/Berlin)\n\nMon 2026-07-06\n  no events\n"),
            "{week}"
        );
        assert!(
            week.contains("Tue 2026-07-07\n  09:00-09:15    Standup  [evoqs/Calendar]\n"),
            "{week}"
        );
        assert_eq!(week.matches("no events").count(), 6, "{week}");
        let day = render_view(
            Layout::Day,
            (today(), today()),
            today(),
            berlin(),
            &[meeting],
            &names(),
        );
        assert_eq!(day, "Wed 2026-07-08  (today)\n  no events\n");
        let agenda = render_view(
            Layout::Agenda,
            (today(), today()),
            today(),
            berlin(),
            &[],
            &names(),
        );
        assert_eq!(agenda, "No events from 2026-07-08 to 2026-07-08\n");
    }

    #[test]
    fn the_calendar_list_aligns_its_columns() {
        let mut calendars = vec![
            CalendarInfo {
                account: "evoqs".into(),
                id: "cal-main".into(),
                name: "Calendar".into(),
                color: Some("#3b82f6".into()),
                is_default: true,
                can_edit: true,
                visible: true,
                backend: "graph".into(),
            },
            CalendarInfo {
                account: "perso".into(),
                id: "imip".into(),
                name: "Invitations".into(),
                color: None,
                is_default: false,
                can_edit: false,
                visible: false,
                backend: "none".into(),
            },
        ];
        assert_eq!(
            render_calendars(&calendars),
            "evoqs  Calendar     cal-main  graph  visible  #3b82f6  (default)\n\
             perso  Invitations  imip      none   hidden   -\n"
        );
        calendars.clear();
        assert_eq!(render_calendars(&calendars), "No calendars\n");
    }

    #[test]
    fn the_event_card_reads_the_times_in_the_viewers_zone() {
        let mut meeting = event(
            "ev-1",
            "Project review",
            timed("2026-07-08T07:00:00Z", "2026-07-08T08:00:00Z"),
        );
        meeting.location = Some("Room 4.12".into());
        meeting.organizer = Some(CalendarPerson {
            name: Some("Ada Lovelace".into()),
            email: "ada@example.com".into(),
        });
        meeting.attendees = vec![
            CalendarAttendee {
                name: None,
                email: "sylvain@example.com".into(),
                role: "required".into(),
                status: "needs-action".into(),
            },
            CalendarAttendee {
                name: Some("Bob".into()),
                email: "bob@example.com".into(),
                role: "optional".into(),
                status: "accepted".into(),
            },
        ];
        meeting.own_response = "needs-action".into();
        meeting.series_master_id = Some("ev-series".into());
        meeting.web_link = Some("https://outlook.example/ev-1".into());
        meeting.body = Some("Agenda attached.\n".into());
        assert_eq!(
            render_event(&meeting, berlin(), &names()),
            "Project review\n\
             \x20 when        Wed 2026-07-08 09:00-10:00 (Europe/Berlin)\n\
             \x20 where       Room 4.12\n\
             \x20 calendar    evoqs/Calendar\n\
             \x20 organizer   Ada Lovelace <ada@example.com>\n\
             \x20 attendees   sylvain@example.com  required, needs-action\n\
             \x20             Bob <bob@example.com>  optional, accepted\n\
             \x20 response    ? needs-action\n\
             \x20 recurrence  one occurrence of a series\n\
             \x20 link        https://outlook.example/ev-1\n\
             \x20 id          ev-1\n\
             \n\
             Agenda attached.\n"
        );
        let tokyo: Tz = "Asia/Tokyo".parse().unwrap();
        assert!(render_event(&meeting, tokyo, &names())
            .contains("when        Wed 2026-07-08 16:00-17:00 (Asia/Tokyo), set in Europe/Berlin"));
        let holiday = event("ev-2", "Holiday", all_day("2026-07-10", "2026-07-12"));
        assert!(render_event(&holiday, berlin(), &names())
            .contains("when        Fri 2026-07-10 to Sat 2026-07-11, all day\n"));
    }

    #[test]
    fn the_server_report_names_each_failed_calendar_or_the_error() {
        // Whether `colored` paints depends on the terminal: compare the text.
        let plain = |lines: Vec<String>| -> Vec<String> {
            lines
                .into_iter()
                .map(|line| {
                    let mut out = String::new();
                    let mut chars = line.chars();
                    while let Some(c) = chars.next() {
                        if c == '\x1b' {
                            chars.by_ref().find(|c| *c == 'm');
                        } else {
                            out.push(c);
                        }
                    }
                    out
                })
                .collect()
        };
        let lines = plain(server_report_lines(&serde_json::json!({
            "calendars": 2, "upserts": 5, "removed": 1, "baselines": 2,
            "failed": [{"calendar": "Shared", "error": "HTTP 403"}],
        })));
        assert_eq!(
            lines,
            [
                "✓ server calendar: 2 calendar(s), 5 event(s) synced, 1 removed, 2 baseline(s)",
                "  ✗ Shared: HTTP 403",
            ]
        );
        let lines = plain(server_report_lines(&serde_json::json!({
            "error": {"code": -32603, "message": "calendar needs a re-login"},
        })));
        assert_eq!(
            lines,
            ["✗ server calendar not synced: calendar needs a re-login"]
        );
    }
}
