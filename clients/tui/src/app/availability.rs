//! The event form's availability rows (PERSO-255, phase 2, slice 7): the
//! free/busy of the user, the attendees and the rooms over the form's day,
//! which `calendar.freebusy` answers and the form shows under its fields.
//!
//! The form's query is its account, its addresses as typed (every one that
//! reads as an address), its day and its zone ([`FreeBusyKey`]). The idle
//! loop asks again once that query has stood still for
//! [`AVAILABILITY_DEBOUNCE`], so typing an address asks once rather than per
//! keystroke, and only on an account whose calendars `calendar.list` lists
//! with `free-busy`. An answer for a query the form no longer names is
//! dropped, and a closed form forgets its rows.

use std::time::{Duration, Instant};

use chrono::NaiveDate;
use mp_protocol::calendar::{CalendarFreeBusyParams, CalendarFreeBusyResult};

use super::{Action, App, EventForm, Overlay};

/// How long the form's query stands still before it is asked.
pub const AVAILABILITY_DEBOUNCE: Duration = Duration::from_millis(500);

/// The slot length the form asks for.
pub const AVAILABILITY_INTERVAL: u32 = 30;

/// What the form asks free/busy about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreeBusyKey {
    pub account: String,
    /// The attendees, the optional ones and the rooms, in the form's order.
    pub addresses: Vec<String>,
    pub day: NaiveDate,
    pub zone: String,
}

impl FreeBusyKey {
    /// `calendar.freebusy`'s params: the whole day in the form's zone.
    pub fn params(&self) -> CalendarFreeBusyParams {
        CalendarFreeBusyParams {
            account: self.account.clone(),
            addresses: self.addresses.clone(),
            start: self.day.to_string(),
            end: self.day.to_string(),
            zone: Some(self.zone.clone()),
            interval_minutes: Some(AVAILABILITY_INTERVAL),
        }
    }
}

/// The availability of the open form.
#[derive(Debug, Clone, Default)]
pub struct Availability {
    /// The query the form names now, and since when.
    pub seen: Option<(FreeBusyKey, Instant)>,
    /// The query asked last.
    pub asked: Option<FreeBusyKey>,
    /// Its operation, while it runs.
    pub operation: Option<String>,
    /// The answer to `asked`, or why there is none.
    pub answer: Option<Result<CalendarFreeBusyResult, String>>,
}

impl Availability {
    /// Whether a query is on its way.
    pub fn checking(&self) -> bool {
        self.operation.is_some()
    }
}

/// Whether `address` reads as a whole address, so a half-typed one is not
/// asked about.
fn complete(address: &str) -> bool {
    match address.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && !address.contains(char::is_whitespace)
        }
        None => false,
    }
}

impl EventForm {
    /// The free/busy query the form names, or `None` while its date does
    /// not read.
    pub fn free_busy_key(&self) -> Option<FreeBusyKey> {
        let day = NaiveDate::parse_from_str(self.date.trim(), "%Y-%m-%d").ok()?;
        let mut addresses: Vec<String> = Vec::new();
        for list in [&self.to, &self.optional, &self.rooms] {
            for address in list
                .split([',', ';'])
                .map(str::trim)
                .filter(|a| complete(a))
            {
                if !addresses.iter().any(|a| a.eq_ignore_ascii_case(address)) {
                    addresses.push(address.to_string());
                }
            }
        }
        Some(FreeBusyKey {
            account: self.chosen().account.clone(),
            addresses,
            day,
            zone: self.zone.clone(),
        })
    }
}

impl App {
    /// Whether `account`'s calendars serve free/busy, as `calendar.list`
    /// last said.
    pub fn offers_free_busy(&self, account: &str) -> bool {
        self.calendar_view
            .calendars
            .iter()
            .any(|c| c.account == account && c.features.iter().any(|f| f == "free-busy"))
    }

    /// Follow the open form's query and ask it once it has stood still for
    /// [`AVAILABILITY_DEBOUNCE`]; a closed form, or one on an account
    /// without free/busy, forgets the rows. Answers whether the rows moved.
    pub fn tick_availability(&mut self, now: Instant) -> bool {
        let key = match &self.overlay {
            Overlay::EventForm(form) => form
                .free_busy_key()
                .filter(|key| self.offers_free_busy(&key.account)),
            _ => None,
        };
        let Some(key) = key else {
            let had = self.calendar_view.availability.seen.is_some()
                || self.calendar_view.availability.answer.is_some();
            self.calendar_view.availability = Availability::default();
            return had;
        };
        // A query the daemon forgot (it restarted) is asked again.
        if let Some(operation) = &self.calendar_view.availability.operation {
            if !self.events.awaits(operation) {
                let state = &mut self.calendar_view.availability;
                state.operation = None;
                state.asked = None;
            }
        }
        let state = &mut self.calendar_view.availability;
        match &state.seen {
            Some((seen, _)) if *seen == key => {}
            _ => {
                state.seen = Some((key, now));
                return false;
            }
        }
        let (seen, since) = state.seen.clone().expect("set above");
        if state.asked.as_ref() == Some(&seen) || now.duration_since(since) < AVAILABILITY_DEBOUNCE
        {
            return false;
        }
        state.asked = Some(seen.clone());
        self.push_action(Action::QueryFreeBusy(Box::new(seen.params())));
        true
    }

    /// `calendar.freebusy` settled: the rows of the query still asked, an
    /// answer for an older one dropped.
    pub fn land_free_busy(
        &mut self,
        operation_id: &str,
        result: Result<CalendarFreeBusyResult, String>,
    ) {
        let state = &mut self.calendar_view.availability;
        if state.operation.as_deref() != Some(operation_id) {
            return;
        }
        state.operation = None;
        state.answer = Some(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::EventCalendarChoice;

    fn form() -> EventForm {
        let calendars = vec![EventCalendarChoice {
            account: "evoqs".into(),
            id: "cal-main".into(),
            name: "Calendar".into(),
            teams: true,
        }];
        let day = NaiveDate::from_ymd_opt(2026, 11, 18).unwrap();
        EventForm::new(calendars, "evoqs", day, "Europe/Berlin")
    }

    /// The query is the account, every whole address of the three lists
    /// once, the day and the zone; a half-typed address waits.
    #[test]
    fn the_query_takes_every_whole_address_once() {
        let mut form = form();
        form.to = "ada@example.com, bob@exa".into();
        form.optional = "Ada@Example.com; kim@example.com".into();
        form.rooms = "room-1@example.com".into();
        let key = form.free_busy_key().unwrap();
        assert_eq!(
            key.addresses,
            ["ada@example.com", "kim@example.com", "room-1@example.com"]
        );
        let params = key.params();
        assert_eq!(params.start, "2026-11-18");
        assert_eq!(params.end, "2026-11-18");
        assert_eq!(params.zone.as_deref(), Some("Europe/Berlin"));
        assert_eq!(params.interval_minutes, Some(30));
        form.date = "2026-11".into();
        assert_eq!(form.free_busy_key(), None);
    }
}
