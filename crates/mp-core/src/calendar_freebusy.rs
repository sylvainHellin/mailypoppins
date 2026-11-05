//! Free/busy (PERSO-255, `.agents/plans/calendar.md`, phase 2, slice 7): when
//! the people and the rooms of a meeting are busy, which the event form
//! shows while the user picks a time and `mp calendar freebusy` prints.
//!
//! [`resolve_query`] turns `calendar.freebusy`'s params into a
//! [`FreeBusyQuery`]: the range as UTC instants, the slot length, and the
//! addresses with the account's own first, each once, within the server's
//! limits (Graph's `getSchedule`: at most 62 days, which mp keeps to
//! [`MAX_RANGE_DAYS`] calendar days, and slots of 5 to 1440 minutes; mp asks
//! for at most [`MAX_ADDRESSES`]). A backend answers one
//! [`Schedule`] per address: the view, one digit per slot as Graph's
//! `availabilityView` spells it, and the blocks that are not free; an address
//! the server has no free/busy for (outside the organisation, unknown) is a
//! schedule with its `error`, so the others still stand.

use chrono::{
    DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, SecondsFormat, TimeZone, Utc,
};
use chrono_tz::Tz;
use mp_protocol::calendar::{
    CalendarFreeBusyParams, CalendarFreeBusyResult, CalendarSchedule, CalendarScheduleItem,
};
use serde::{Deserialize, Serialize};

use crate::calendar_model::{parse_zone, CalendarWindow, ShowAs};
use crate::invite::{parse_date, parse_datetime_in};

/// The most addresses one query asks about, the account's own included.
pub const MAX_ADDRESSES: usize = 20;

/// The longest range one query covers, in calendar days of the query's
/// zone, so the clocks changing inside it neither shorten nor lengthen it.
/// Graph's `getSchedule` takes 62 days (62 pass live, 63 are refused with
/// `ErrorTimeIntervalTooBig`) and can only measure them as elapsed time,
/// since the request carries UTC instants; 62 calendar days across the
/// autumn change are 62 days and an hour, which it may refuse. One day less
/// keeps any run of calendar days inside Graph's limit, whatever the clocks
/// do in between (a change is at most two hours).
pub const MAX_RANGE_DAYS: i64 = 61;

/// The slot length when the params name none, the event form's.
pub const DEFAULT_INTERVAL: u32 = 30;

/// The slot lengths Graph takes (`ErrorInvalidMergedFreeBusyInterval`
/// outside them).
pub const MIN_INTERVAL: u32 = 5;
pub const MAX_INTERVAL: u32 = 1440;

/// One `calendar.freebusy`, resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreeBusyQuery {
    /// The account's own address first, then the ones asked, each once.
    pub addresses: Vec<String>,
    pub window: CalendarWindow,
    /// Minutes per slot of a schedule's view.
    pub interval: u32,
}

impl FreeBusyQuery {
    /// How many slots a view of this query has: the range over the slot
    /// length, a last partial slot included.
    pub fn slots(&self) -> usize {
        let minutes = (self.window.end - self.window.start).num_minutes().max(0) as u64;
        minutes.div_ceil(u64::from(self.interval.max(1))) as usize
    }
}

/// A block of a schedule that is not free.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleItem {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub status: ShowAs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

/// One address's free/busy over a query's range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub address: String,
    /// One [`slot_digit`] per slot; empty with `error`.
    pub view: String,
    pub items: Vec<ScheduleItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Schedule {
    /// The schedule of an address the server has no free/busy for.
    pub fn unavailable(address: &str, why: impl Into<String>) -> Schedule {
        Schedule {
            address: address.to_string(),
            view: String::new(),
            items: Vec::new(),
            error: Some(why.into()),
        }
    }

    /// The schedule of `items` over `query`, its view computed from them, as
    /// a backend without a view of its own (the fake) answers.
    pub fn from_items(address: &str, mut items: Vec<ScheduleItem>, query: &FreeBusyQuery) -> Self {
        items.retain(|item| item.end > query.window.start && item.start < query.window.end);
        items.sort_by_key(|item| item.start);
        let view = view_of(&items, query);
        Schedule {
            address: address.to_string(),
            view,
            items,
            error: None,
        }
    }

    /// The wire form, times as RFC 3339 UTC.
    pub fn to_wire(&self) -> CalendarSchedule {
        CalendarSchedule {
            address: self.address.clone(),
            view: self.view.clone(),
            items: self
                .items
                .iter()
                .map(|item| CalendarScheduleItem {
                    start: rfc3339(item.start),
                    end: rfc3339(item.end),
                    status: show_as_word(item.status).to_string(),
                    subject: item.subject.clone(),
                    location: item.location.clone(),
                })
                .collect(),
            error: self.error.clone(),
        }
    }
}

/// The digit a slot of `status` reads as in a view, Graph's
/// `availabilityView`: `0` free, `1` tentative, `2` busy, `3` away, `4`
/// working elsewhere, `?` unknown.
pub fn slot_digit(status: ShowAs) -> char {
    match status {
        ShowAs::Free => '0',
        ShowAs::Tentative => '1',
        ShowAs::Busy => '2',
        ShowAs::Oof => '3',
        ShowAs::WorkingElsewhere => '4',
        ShowAs::Unknown => '?',
    }
}

/// The status a view's digit stands for; anything else is unknown.
pub fn slot_status(digit: char) -> ShowAs {
    match digit {
        '0' => ShowAs::Free,
        '1' => ShowAs::Tentative,
        '2' => ShowAs::Busy,
        '3' => ShowAs::Oof,
        '4' => ShowAs::WorkingElsewhere,
        _ => ShowAs::Unknown,
    }
}

/// The word a status travels as, the model's kebab-case.
pub fn show_as_word(status: ShowAs) -> &'static str {
    match status {
        ShowAs::Free => "free",
        ShowAs::Tentative => "tentative",
        ShowAs::Busy => "busy",
        ShowAs::Oof => "oof",
        ShowAs::WorkingElsewhere => "working-elsewhere",
        ShowAs::Unknown => "unknown",
    }
}

/// Which of two statuses a slot shows when both cover it: away over busy
/// over tentative over working elsewhere over free.
fn weight(status: ShowAs) -> u8 {
    match status {
        ShowAs::Free => 0,
        ShowAs::Unknown => 1,
        ShowAs::WorkingElsewhere => 2,
        ShowAs::Tentative => 3,
        ShowAs::Busy => 4,
        ShowAs::Oof => 5,
    }
}

/// The view of `items` over `query`: each slot the heaviest status of the
/// items overlapping it, free where none does.
pub fn view_of(items: &[ScheduleItem], query: &FreeBusyQuery) -> String {
    let step = Duration::minutes(i64::from(query.interval.max(1)));
    (0..query.slots())
        .map(|slot| {
            let from = query.window.start + step * slot as i32;
            let to = (from + step).min(query.window.end);
            items
                .iter()
                .filter(|item| item.start < to && item.end > from)
                .map(|item| item.status)
                .max_by_key(|status| weight(*status))
                .map_or('0', slot_digit)
        })
        .collect()
}

/// The [`FreeBusyQuery`] `params` names, `own` (the account's address)
/// first, or the sentence that says what is wrong with it. Dates and times
/// without an offset are read in `params.zone`, else in `host`.
pub fn resolve_query(
    params: &CalendarFreeBusyParams,
    own: &str,
    host: Tz,
) -> Result<FreeBusyQuery, String> {
    let zone = match params.zone.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() => parse_zone(name).ok_or_else(|| {
            format!("unknown time zone {name:?}; use an IANA name like Europe/Berlin")
        })?,
        _ => host,
    };
    let (start, start_wall) =
        bound(&params.start, zone, false).map_err(|e| format!("start: {e}"))?;
    let (end, end_wall) = bound(&params.end, zone, true).map_err(|e| format!("end: {e}"))?;
    if end <= start {
        return Err("The end of the free/busy range must come after its start".into());
    }
    // The range's length on the zone's clock: a day is a day, 23 or 25
    // hours long on the days the clocks change.
    let span = end_wall - start_wall;
    if span > Duration::days(MAX_RANGE_DAYS) {
        return Err(format!(
            "Free/busy covers at most {MAX_RANGE_DAYS} days at a time"
        ));
    }
    let interval = params.interval_minutes.unwrap_or(DEFAULT_INTERVAL);
    if !(MIN_INTERVAL..=MAX_INTERVAL).contains(&interval) {
        return Err(format!(
            "The slot length is {MIN_INTERVAL} to {MAX_INTERVAL} minutes, not {interval}"
        ));
    }
    if i64::from(interval) > span.num_minutes() {
        return Err(format!(
            "A slot of {interval} minutes is longer than the free/busy range"
        ));
    }
    let mut addresses: Vec<String> = Vec::new();
    let own = own.trim();
    if own.contains('@') {
        addresses.push(own.to_string());
    }
    for address in &params.addresses {
        let address = address.trim();
        if address.is_empty() {
            continue;
        }
        if !address.contains('@') || address.contains(char::is_whitespace) {
            return Err(format!("{address:?} is not an email address"));
        }
        if !addresses
            .iter()
            .any(|known| known.eq_ignore_ascii_case(address))
        {
            addresses.push(address.to_string());
        }
    }
    if addresses.is_empty() {
        return Err("A free/busy query needs at least one address".into());
    }
    if addresses.len() > MAX_ADDRESSES {
        return Err(format!(
            "Free/busy asks about at most {MAX_ADDRESSES} addresses at a time, the account's \
             own included"
        ));
    }
    Ok(FreeBusyQuery {
        addresses,
        window: CalendarWindow { start, end },
        interval,
    })
}

/// One end of the range, as an instant and as the wall time it reads in
/// `zone`: a bare date's midnight, the midnight after it for the `end` (the
/// last day, included), or a time.
fn bound(raw: &str, zone: Tz, end: bool) -> Result<(DateTime<Utc>, NaiveDateTime), String> {
    match parse_date(raw) {
        Some(date) => {
            let date = if end {
                date.succ_opt()
                    .ok_or_else(|| "the date runs past the calendar".to_string())?
            } else {
                date
            };
            Ok((midnight(date, zone)?, date.and_time(NaiveTime::MIN)))
        }
        None => {
            let at = parse_datetime_in(raw, zone).map_err(|e| format!("{e:#}"))?;
            Ok((at, at.with_timezone(&zone).naive_local()))
        }
    }
}

/// The first instant of `date` in `zone`.
fn midnight(date: NaiveDate, zone: Tz) -> Result<DateTime<Utc>, String> {
    let local = date.and_time(NaiveTime::MIN);
    zone.from_local_datetime(&local)
        .earliest()
        .or_else(|| {
            // A zone whose clocks skip midnight starts the day an hour later.
            zone.from_local_datetime(&(local + Duration::hours(1)))
                .earliest()
        })
        .map(|at| at.with_timezone(&Utc))
        .ok_or_else(|| format!("{date} has no midnight in {}", zone.name()))
}

/// What a settled `calendar.freebusy` reports.
pub fn to_result(
    account: &str,
    query: &FreeBusyQuery,
    schedules: &[Schedule],
) -> CalendarFreeBusyResult {
    CalendarFreeBusyResult {
        account: account.to_string(),
        start: rfc3339(query.window.start),
        end: rfc3339(query.window.end),
        interval_minutes: query.interval,
        schedules: schedules.iter().map(Schedule::to_wire).collect(),
    }
}

fn rfc3339(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn berlin() -> Tz {
        parse_zone("Europe/Berlin").unwrap()
    }

    fn params(addresses: &[&str], start: &str, end: &str) -> CalendarFreeBusyParams {
        CalendarFreeBusyParams {
            account: "evoqs".into(),
            addresses: addresses.iter().map(|a| a.to_string()).collect(),
            start: start.into(),
            end: end.into(),
            zone: Some("Europe/Berlin".into()),
            interval_minutes: None,
        }
    }

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// A day is its two midnights in the zone, the account first and each
    /// address once whatever its case, in 30-minute slots.
    #[test]
    fn a_day_query_asks_the_account_first_and_each_address_once() {
        let query = resolve_query(
            &params(
                &[
                    "ada@example.com",
                    " ",
                    "S.Hellin@evoqs.com",
                    "Ada@example.com",
                ],
                "2026-11-18",
                "2026-11-18",
            ),
            "s.hellin@evoqs.com",
            chrono_tz::UTC,
        )
        .unwrap();
        assert_eq!(query.addresses, ["s.hellin@evoqs.com", "ada@example.com"]);
        assert_eq!(query.window.start, utc("2026-11-17T23:00:00Z"));
        assert_eq!(query.window.end, utc("2026-11-18T23:00:00Z"));
        assert_eq!(query.interval, 30);
        assert_eq!(query.slots(), 48);
    }

    /// Times are read in the zone, or keep their offset.
    #[test]
    fn times_are_read_in_the_zone() {
        let mut asked = params(&[], "2026-11-18T07:00", "2026-11-18T20:00:00+01:00");
        asked.interval_minutes = Some(60);
        let query = resolve_query(&asked, "me@example.com", berlin()).unwrap();
        assert_eq!(query.window.start, utc("2026-11-18T06:00:00Z"));
        assert_eq!(query.window.end, utc("2026-11-18T19:00:00Z"));
        assert_eq!(query.slots(), 13);
    }

    /// Graph's limits are refused before any call: 61 days pass and 62 do
    /// not, a slot is 5 to 1440 minutes and no longer than the range, and
    /// at most twenty addresses go, the account's own among them.
    #[test]
    fn the_servers_limits_are_refused_before_the_call() {
        let own = "me@example.com";
        let host = berlin();
        assert!(resolve_query(&params(&[], "2026-11-01", "2026-12-31"), own, host).is_ok());
        let err = resolve_query(&params(&[], "2026-11-01", "2027-01-01"), own, host).unwrap_err();
        assert!(err.contains("61 days"), "{err}");
        let err = resolve_query(&params(&[], "2026-11-18", "2026-11-17"), own, host).unwrap_err();
        assert!(err.contains("after its start"), "{err}");
        for (interval, words) in [(4, "5 to 1440"), (1441, "5 to 1440")] {
            let mut asked = params(&[], "2026-11-18", "2026-11-18");
            asked.interval_minutes = Some(interval);
            let err = resolve_query(&asked, own, host).unwrap_err();
            assert!(err.contains(words), "{err}");
        }
        let mut asked = params(&[], "2026-11-18T09:00", "2026-11-18T09:10");
        asked.interval_minutes = Some(30);
        assert!(resolve_query(&asked, own, host)
            .unwrap_err()
            .contains("longer"));
        let many: Vec<String> = (0..20).map(|i| format!("p{i}@example.com")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let err = resolve_query(&params(&many, "2026-11-18", "2026-11-18"), own, host).unwrap_err();
        assert!(err.contains("at most 20"), "{err}");
        assert!(resolve_query(&params(&many[..19], "2026-11-18", "2026-11-18"), own, host).is_ok());
        let err =
            resolve_query(&params(&["ada"], "2026-11-18", "2026-11-18"), own, host).unwrap_err();
        assert!(err.contains("not an email address"), "{err}");
        let err = resolve_query(&params(&[], "soon", "2026-11-18"), own, host).unwrap_err();
        assert!(err.starts_with("start:"), "{err}");
        let mut asked = params(&[], "2026-11-18", "2026-11-18");
        asked.zone = Some("Mars/Olympus".into());
        assert!(resolve_query(&asked, own, host)
            .unwrap_err()
            .contains("unknown time zone"));
    }

    /// The range is counted in the zone's calendar days, so the clocks
    /// changing inside it move neither limit: 61 days across the autumn
    /// change (61 days and an hour) pass, and so does a slot of a whole day
    /// on the spring change's 23-hour day; every range that passes stays
    /// within Graph's 62 days of elapsed time.
    #[test]
    fn the_limits_count_calendar_days_across_a_clock_change() {
        let own = "me@example.com";
        let host = berlin();
        for (first, last) in [
            ("2026-09-01", "2026-10-31"),
            ("2026-03-01", "2026-04-30"),
            ("2026-09-01T00:00", "2026-11-01T00:00"),
        ] {
            let query = resolve_query(&params(&[], first, last), own, host)
                .unwrap_or_else(|err| panic!("{first} to {last}: {err}"));
            assert!(query.window.end - query.window.start < Duration::days(62));
        }
        let autumn = resolve_query(&params(&[], "2026-09-01", "2026-10-31"), own, host).unwrap();
        assert_eq!(
            autumn.window.end - autumn.window.start,
            Duration::days(61) + Duration::hours(1)
        );
        for (first, last) in [
            ("2026-09-01", "2026-11-01"),
            ("2026-03-01", "2026-05-01"),
            ("2026-09-01T00:00", "2026-11-01T00:01"),
        ] {
            let err = resolve_query(&params(&[], first, last), own, host).unwrap_err();
            assert!(err.contains("61 days"), "{first} to {last}: {err}");
        }
        let mut asked = params(&[], "2026-03-29", "2026-03-29");
        asked.interval_minutes = Some(1440);
        let query = resolve_query(&asked, own, host).unwrap();
        assert_eq!(query.window.end - query.window.start, Duration::hours(23));
        assert_eq!(query.slots(), 1);
    }

    /// The view of a fake's items is each slot's heaviest status, a partial
    /// last slot included.
    #[test]
    fn a_view_is_each_slots_heaviest_status() {
        let query = FreeBusyQuery {
            addresses: vec!["me@example.com".into()],
            window: CalendarWindow {
                start: utc("2026-11-18T08:00:00Z"),
                end: utc("2026-11-18T10:15:00Z"),
            },
            interval: 30,
        };
        let item = |from: &str, to: &str, status| ScheduleItem {
            start: utc(from),
            end: utc(to),
            status,
            subject: None,
            location: None,
        };
        let schedule = Schedule::from_items(
            "me@example.com",
            vec![
                item("2026-11-18T09:00:00Z", "2026-11-18T10:00:00Z", ShowAs::Busy),
                item(
                    "2026-11-18T08:00:00Z",
                    "2026-11-18T08:45:00Z",
                    ShowAs::Tentative,
                ),
                item("2026-11-18T09:30:00Z", "2026-11-18T09:45:00Z", ShowAs::Oof),
                item(
                    "2026-11-18T10:10:00Z",
                    "2026-11-18T11:00:00Z",
                    ShowAs::WorkingElsewhere,
                ),
                item("2026-11-19T08:00:00Z", "2026-11-19T09:00:00Z", ShowAs::Busy),
            ],
            &query,
        );
        assert_eq!(schedule.view, "11234");
        assert_eq!(schedule.items.len(), 4, "the item outside the range goes");
        assert_eq!(schedule.items[0].status, ShowAs::Tentative);
        let wire = schedule.to_wire();
        assert_eq!(wire.items[0].start, "2026-11-18T08:00:00Z");
        assert_eq!(wire.items[3].status, "working-elsewhere");
        assert_eq!(wire.error, None);
        assert_eq!(
            Schedule::unavailable("x@gmail.com", "no").to_wire().view,
            ""
        );
        for status in [
            ShowAs::Free,
            ShowAs::Tentative,
            ShowAs::Busy,
            ShowAs::Oof,
            ShowAs::WorkingElsewhere,
            ShowAs::Unknown,
        ] {
            assert_eq!(slot_status(slot_digit(status)), status);
        }
    }
}
