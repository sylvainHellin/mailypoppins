//! The Calendar view's model (#0034, PERSO-200): which days a layout shows,
//! what part of a day an event covers, the cursor's rows, and the reads and
//! writes the view makes through the daemon (`calendar.view`, `calendar.list`,
//! `calendar.set_visibility`).
//!
//! The pure half ([`day_part`], [`calendar_entries`]) takes the events, the
//! days and the zone and nothing else, so the layouts the renderer draws are
//! tested without a terminal and without a clock.

use chrono::{DateTime, Days, NaiveDate, Utc};
use mp_core::calendar_model::day_start;
use mp_protocol::calendar::{CalendarViewParams, IMIP_EVENT_PREFIX};

use super::{
    App, CalendarEntry, CalendarEvent, CalendarEventTime, CalendarLayout, CalendarsOverlay,
    EventCardOverlay, MessageRef, Overlay, RsvpOverlay, StatusLevel, Tz, View,
};

/// The part of one day an event covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayPart {
    /// An all-day event, or a day of a multi-day one.
    AllDay,
    /// A timed event: its start and end on this day in the view's zone,
    /// clamped to the day's midnights.
    Timed {
        start: DateTime<Tz>,
        end: DateTime<Tz>,
        /// It started on an earlier day.
        from_before: bool,
        /// It runs on past this day's end.
        past_midnight: bool,
    },
}

impl DayPart {
    /// The time column of the agenda: `all day`, or `HH:MM-HH:MM` with
    /// `24:00` where the event runs on past midnight.
    pub fn label(&self) -> String {
        match self {
            DayPart::AllDay => "all day".to_string(),
            DayPart::Timed {
                start,
                end,
                past_midnight,
                ..
            } => {
                let until = if *past_midnight {
                    "24:00".to_string()
                } else {
                    end.format("%H:%M").to_string()
                };
                if start == end && !past_midnight {
                    start.format("%H:%M").to_string()
                } else {
                    format!("{}-{until}", start.format("%H:%M"))
                }
            }
        }
    }

    /// The clock of the start, `HH:MM`, for the short blocks of the grids.
    pub fn start_clock(&self) -> String {
        match self {
            DayPart::AllDay => String::new(),
            DayPart::Timed { start, .. } => start.format("%H:%M").to_string(),
        }
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

/// What part of `day` in `zone` `event` covers, or `None` when it does not
/// fall on that day. The same rule as `mp calendar view`: an all-day event
/// covers its dates up to the exclusive end, a timed one every day its
/// half-open interval touches, and a zero-length one the day of its start.
pub fn day_part(event: &CalendarEvent, day: NaiveDate, zone: Tz) -> Option<DayPart> {
    match &event.time {
        CalendarEventTime::AllDay { start, end } => {
            let (start, end) = (date(start)?, date(end)?);
            let covers = if end > start {
                start <= day && day < end
            } else {
                day == start
            };
            covers.then_some(DayPart::AllDay)
        }
        CalendarEventTime::Timed { start, end, .. } => {
            let (start, end) = (instant(start)?, instant(end)?);
            let (open, close) = (day_start(zone, day), day_start(zone, day + Days::new(1)));
            if end <= start {
                return (open <= start && start < close).then(|| DayPart::Timed {
                    start: start.with_timezone(&zone),
                    end: start.with_timezone(&zone),
                    from_before: false,
                    past_midnight: false,
                });
            }
            if start >= close || end <= open {
                return None;
            }
            Some(DayPart::Timed {
                start: start.max(open).with_timezone(&zone),
                end: end.min(close).with_timezone(&zone),
                from_before: start < open,
                past_midnight: end > close,
            })
        }
    }
}

/// The cursor's rows for `layout` anchored on `anchor`: each event under
/// every shown day it covers, day by day, the all-day ones first on their day
/// and the rest in `calendar.view`'s order.
pub fn calendar_entries(
    layout: CalendarLayout,
    anchor: NaiveDate,
    events: &[CalendarEvent],
    zone: Tz,
) -> Vec<CalendarEntry> {
    let (first, last) = layout.days(anchor);
    let mut entries = Vec::new();
    for day in first.iter_days().take_while(|day| *day <= last) {
        let mut on_day: Vec<(bool, usize)> = events
            .iter()
            .enumerate()
            .filter_map(|(i, event)| {
                day_part(event, day, zone).map(|part| (part != DayPart::AllDay, i))
            })
            .collect();
        // Stable: all-day first, the daemon's order kept within each half.
        on_day.sort_by_key(|(timed, _)| *timed);
        entries.extend(
            on_day
                .into_iter()
                .map(|(_, event)| CalendarEntry { day, event }),
        );
    }
    entries
}

/// The `calendar.view` parameters of the days `first` to `last` in `zone`.
fn view_params(first: NaiveDate, last: NaiveDate, zone: Tz) -> CalendarViewParams {
    let start = day_start(zone, first).with_timezone(&zone);
    let end = day_start(zone, last + Days::new(1)).with_timezone(&zone);
    CalendarViewParams {
        start: start.to_rfc3339(),
        end: end.to_rfc3339(),
        zone: Some(zone.name().to_string()),
        ..Default::default()
    }
}

impl App {
    /// Load the view the first time the Calendar view is shown.
    pub fn ensure_calendar_loaded(&mut self) {
        if !self.calendar_view.loaded {
            self.load_calendar();
        }
    }

    /// Read the shown days' events and every calendar from the daemon, and
    /// keep the cursor on the event it was on when that event is still shown.
    ///
    /// Synchronous on the UI thread, like the Contacts load: two queries over
    /// the daemon's cache, which never reach a server. An `App` with no
    /// session shows an empty view rather than reading anything itself.
    pub fn load_calendar(&mut self) {
        let zone = self.calendar_view.zone;
        self.calendar_view.today = Utc::now().with_timezone(&zone).date_naive();
        let (first, last) = self.calendar_view.layout.days(self.calendar_view.anchor);
        let (events, calendars, error) = match self.queries() {
            None => (Vec::new(), Vec::new(), None),
            Some(queries) => {
                let params = view_params(first, last, zone);
                let calendars = super::super::queries::calendar_list(queries);
                match super::super::queries::calendar_view(queries, &params) {
                    Ok(view) => (view.events, calendars, None),
                    Err(e) => (Vec::new(), calendars, Some(format!("{e:#}"))),
                }
            }
        };
        if let Some(error) = &error {
            log::warn!("[queries] calendar.view from {first} to {last}: {error}");
        }
        let kept = self.selected_calendar_key();
        self.calendar_view.events = events;
        self.calendar_view.calendars = calendars;
        self.calendar_view.error = error;
        self.calendar_view.loaded = true;
        self.recompute_calendar_entries(kept);
    }

    /// [`Self::load_calendar`] with the count on the status line: the
    /// Calendar view's `r`.
    pub fn refresh_calendar(&mut self) {
        self.load_calendar();
        match &self.calendar_view.error {
            Some(error) => self.set_status_level(format!("Calendar: {error}"), StatusLevel::Error),
            None => {
                let count = self.calendar_view.events.len();
                self.set_status(format!("Calendar refreshed ({count} events)"));
            }
        }
    }

    /// Reload the view in place when it is holding one, and say nothing: a
    /// mutation or a daemon event that moved the events has already put its
    /// own line on the status bar.
    pub fn rebuild_calendar_if_loaded(&mut self) {
        if self.calendar_view.loaded {
            self.load_calendar();
        }
    }

    /// `calendar.changed` (PERSO-200): a sync, an answer or a visibility
    /// change moved an account's calendar, here or in another client. The
    /// view reads its days again when it holds them, and an open calendars
    /// overlay its list.
    pub fn on_calendar_changed(&mut self) {
        self.rebuild_calendar_if_loaded();
        if matches!(self.overlay, Overlay::Calendars(_)) {
            let calendars = self.calendar_view.calendars.clone();
            if let Overlay::Calendars(overlay) = &mut self.overlay {
                overlay.selected = overlay.selected.min(calendars.len().saturating_sub(1));
                overlay.calendars = calendars;
            }
        }
    }

    /// The account, id and day of the entry under the cursor, which a reload
    /// puts the cursor back on.
    fn selected_calendar_key(&self) -> Option<(String, String, NaiveDate)> {
        let entry = self.selected_calendar_entry()?;
        let event = self.calendar_view.events.get(entry.event)?;
        Some((event.account.clone(), event.id.clone(), entry.day))
    }

    /// Recompute the cursor's rows from the events, back on `kept` when it
    /// is still shown (on the same day, else on any), else clamped.
    pub fn recompute_calendar_entries(&mut self, kept: Option<(String, String, NaiveDate)>) {
        let cv = &mut self.calendar_view;
        cv.entries = calendar_entries(cv.layout, cv.anchor, &cv.events, cv.zone);
        let same = |entry: &CalendarEntry, account: &str, id: &str| {
            cv.events
                .get(entry.event)
                .is_some_and(|event| event.account == account && event.id == id)
        };
        let found = kept.and_then(|(account, id, day)| {
            cv.entries
                .iter()
                .position(|entry| entry.day == day && same(entry, &account, &id))
                .or_else(|| {
                    cv.entries
                        .iter()
                        .position(|entry| same(entry, &account, &id))
                })
        });
        let len = cv.entries.len();
        cv.list_index = match found {
            Some(index) => index,
            None if len == 0 => 0,
            None => cv.list_index.min(len - 1),
        };
    }

    /// The row under the Calendar cursor, if any.
    pub fn selected_calendar_entry(&self) -> Option<CalendarEntry> {
        self.calendar_view
            .entries
            .get(self.calendar_view.list_index)
            .copied()
    }

    /// The event under the Calendar cursor, if any.
    pub fn selected_event(&self) -> Option<&CalendarEvent> {
        let entry = self.selected_calendar_entry()?;
        self.calendar_view.events.get(entry.event)
    }

    /// Put the cursor on the first row of `day` or after it, else on the
    /// last row: where a move to another day or week lands.
    fn calendar_cursor_to(&mut self, day: NaiveDate) {
        let cv = &mut self.calendar_view;
        cv.list_index = cv
            .entries
            .iter()
            .position(|entry| entry.day >= day)
            .unwrap_or(cv.entries.len().saturating_sub(1));
    }

    /// Show the view anchored on `day`, the cursor on its first event.
    pub fn calendar_go_to(&mut self, day: NaiveDate) {
        self.calendar_view.anchor = day;
        self.load_calendar();
        self.calendar_cursor_to(day);
    }

    /// `h`/`l`: the previous or the next day or week.
    pub fn calendar_shift(&mut self, forward: bool) {
        let step = Days::new(self.calendar_view.layout.step());
        let anchor = self.calendar_view.anchor;
        let day = if forward {
            anchor.checked_add_days(step)
        } else {
            anchor.checked_sub_days(step)
        };
        if let Some(day) = day {
            self.calendar_go_to(day);
        }
    }

    /// Switch to `layout`, anchored on the day under the cursor so the view
    /// keeps showing what the user was looking at.
    pub fn calendar_set_layout(&mut self, layout: CalendarLayout) {
        if let Some(entry) = self.selected_calendar_entry() {
            self.calendar_view.anchor = entry.day;
        }
        self.calendar_view.layout = layout;
        let kept = self.selected_calendar_key();
        self.load_calendar();
        if kept.is_none() {
            let anchor = self.calendar_view.anchor;
            self.calendar_cursor_to(anchor);
        }
        self.set_status(format!("Calendar: {} layout", layout.label()));
    }

    /// The accounts whose server calendar needs a sign-in with the calendar
    /// scope, by name, from `account.credentials` (PERSO-200).
    pub fn calendar_scope_missing(&self) -> Vec<String> {
        self.accounts
            .iter()
            .filter(|state| {
                state
                    .credentials
                    .is_some_and(|credentials| credentials.calendar_scope_missing)
            })
            .map(|state| state.account_config.name.clone())
            .collect()
    }

    /// `c`: the calendars overlay over the list the view last read.
    pub fn open_calendars_overlay(&mut self) {
        if let Some(queries) = self.queries() {
            self.calendar_view.calendars = super::super::queries::calendar_list(queries);
        }
        if self.calendar_view.calendars.is_empty() {
            self.set_status(
                "No calendars: no account has a server calendar or an invitation".into(),
            );
            return;
        }
        self.overlay = Overlay::Calendars(CalendarsOverlay {
            calendars: self.calendar_view.calendars.clone(),
            selected: 0,
        });
    }

    /// `Space` in the calendars overlay: flip the visibility of the calendar
    /// under the cursor through `calendar.set_visibility`, then read the view
    /// again. The preference is the daemon's, so nothing is kept here.
    pub fn toggle_calendar_visibility(&mut self) {
        let Overlay::Calendars(overlay) = &self.overlay else {
            return;
        };
        let Some(calendar) = overlay.calendars.get(overlay.selected).cloned() else {
            return;
        };
        let Some(queries) = self.queries() else {
            self.set_status_level("No daemon session".into(), StatusLevel::Error);
            return;
        };
        let visible = !calendar.visible;
        if let Err(e) = super::super::queries::calendar_set_visibility(
            queries,
            &calendar.account,
            &calendar.id,
            visible,
        ) {
            self.set_status_level(
                format!("Cannot change {}: {e:#}", calendar.name),
                StatusLevel::Error,
            );
            return;
        }
        self.load_calendar();
        let calendars = self.calendar_view.calendars.clone();
        if let Overlay::Calendars(overlay) = &mut self.overlay {
            overlay.calendars = calendars;
        }
        let verb = if visible { "Showing" } else { "Hiding" };
        self.set_status(format!("{verb} {}/{}", calendar.account, calendar.name));
    }

    /// `Enter`: the event card of the event under the cursor.
    pub fn open_event_card(&mut self) {
        if let Some(event) = self.selected_event().cloned() {
            self.overlay = Overlay::EventCard(Box::new(EventCardOverlay { event, scroll: 0 }));
        }
    }

    /// The invitation mail behind `event`, when it came from one on the
    /// active account: its row, and the mailbox its selector names.
    fn event_mail(&mut self, event: &CalendarEvent) -> Option<(MessageRef, Option<String>)> {
        let Some(source) = &event.source else {
            self.set_status(
                "This event is the server's; no invitation mail stands behind it".into(),
            );
            return None;
        };
        if event.account != self.account_config.name {
            self.set_status(format!(
                "That invitation is in {}; switch to that account first (ga)",
                event.account
            ));
            return None;
        }
        let mailbox = mp_core::selector::parse(&source.selector)
            .ok()
            .and_then(|parts| parts.mailbox);
        Some((MessageRef::new(source.row_id), mailbox))
    }

    /// `e`: the invitation's `.ics` in `$EDITOR`, for an event that came from
    /// a mail.
    pub fn open_event_ics(&mut self, event: &CalendarEvent) {
        if let Some((msg, _)) = self.event_mail(event) {
            self.push_action(super::Action::OpenEventSource { msg });
        }
    }

    /// `m` on the event card: the invitation mail in the Mail view.
    pub fn open_event_mail(&mut self, event: &CalendarEvent) {
        let Some((msg, mailbox)) = self.event_mail(event) else {
            return;
        };
        let Some(mailbox) = mailbox else {
            self.set_status("The invitation's selector names no mailbox".into());
            return;
        };
        self.close_overlay();
        self.switch_view(View::Mail);
        self.open_message(msg, &mailbox);
    }

    /// `V` on an event: the respond overlay, unless there is nothing to
    /// answer. Which answers the daemon takes is its own business; what is
    /// refused here is what no answer could mean.
    pub fn open_rsvp_overlay_for(&mut self, event: &CalendarEvent) {
        if event.cancelled {
            self.set_status(
                "This event was cancelled by the organizer \u{2014} nothing to RSVP".to_string(),
            );
            return;
        }
        if event.is_organizer || event.own_response == "organizer" {
            self.set_status("You are the organizer of this invite \u{2014} nothing to RSVP".into());
            return;
        }
        if event.own_response == "none" && !event.id.starts_with(IMIP_EVENT_PREFIX) {
            self.set_status("This event is not an invitation; there is nothing to answer".into());
            return;
        }
        let summary = if event.summary.trim().is_empty() {
            event
                .source
                .as_ref()
                .map(|source| source.subject.clone())
                .unwrap_or_else(|| "(no title)".to_string())
        } else {
            event.summary.clone()
        };
        self.overlay = Overlay::Rsvp(RsvpOverlay::new(
            event.account.clone(),
            event.id.clone(),
            summary,
        ));
    }

    /// `V` in the Calendar view: answer the event under the cursor.
    pub fn open_rsvp_overlay_for_event(&mut self) {
        if let Some(event) = self.selected_event().cloned() {
            self.open_rsvp_overlay_for(&event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp_protocol::calendar::CalendarEventTime;

    fn berlin() -> Tz {
        "Europe/Berlin".parse().unwrap()
    }

    fn d(raw: &str) -> NaiveDate {
        NaiveDate::parse_from_str(raw, "%Y-%m-%d").unwrap()
    }

    fn event(id: &str, time: CalendarEventTime) -> CalendarEvent {
        serde_json::from_value(serde_json::json!({
            "account": "work", "calendar": "cal", "id": id, "uid": id,
            "summary": id, "time": time, "own_response": "none",
            "sensitivity": "normal", "show_as": "busy",
        }))
        .unwrap()
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

    /// A timed event is read in the view's zone and clamped to each day it
    /// touches; an all-day one covers its dates up to the exclusive end.
    #[test]
    fn a_day_part_reads_the_zone_and_clamps_to_the_day() {
        let meeting = event("m", timed("2026-07-07T07:00:00Z", "2026-07-07T08:30:00Z"));
        assert_eq!(
            day_part(&meeting, d("2026-07-07"), berlin())
                .unwrap()
                .label(),
            "09:00-10:30"
        );
        assert_eq!(day_part(&meeting, d("2026-07-08"), berlin()), None);

        let night = event("n", timed("2026-07-07T20:00:00Z", "2026-07-08T06:00:00Z"));
        assert_eq!(
            day_part(&night, d("2026-07-07"), berlin()).unwrap().label(),
            "22:00-24:00"
        );
        assert_eq!(
            day_part(&night, d("2026-07-08"), berlin()).unwrap().label(),
            "00:00-08:00"
        );

        let holiday = event("h", all_day("2026-07-08", "2026-07-10"));
        assert_eq!(
            day_part(&holiday, d("2026-07-08"), berlin()),
            Some(DayPart::AllDay)
        );
        assert_eq!(
            day_part(&holiday, d("2026-07-09"), berlin()),
            Some(DayPart::AllDay)
        );
        assert_eq!(day_part(&holiday, d("2026-07-10"), berlin()), None);
    }

    /// Each layout's rows: the agenda and the week list an event under every
    /// day it covers, all-day first on its day; the day grid only its day.
    #[test]
    fn the_entries_follow_the_layout_and_put_all_day_first() {
        let events = vec![
            event(
                "early",
                timed("2026-07-07T06:00:00Z", "2026-07-07T07:00:00Z"),
            ),
            event("holiday", all_day("2026-07-07", "2026-07-09")),
            event(
                "late",
                timed("2026-07-10T15:00:00Z", "2026-07-10T16:00:00Z"),
            ),
        ];
        let ids = |layout, anchor| {
            calendar_entries(layout, d(anchor), &events, berlin())
                .into_iter()
                .map(|entry| format!("{} {}", entry.day.format("%d"), events[entry.event].id))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(CalendarLayout::Week, "2026-07-09"),
            ["07 holiday", "07 early", "08 holiday", "10 late"]
        );
        assert_eq!(
            ids(CalendarLayout::Day, "2026-07-07"),
            ["07 holiday", "07 early"]
        );
        assert_eq!(
            ids(CalendarLayout::Agenda, "2026-07-08"),
            ["08 holiday", "10 late"]
        );
    }

    /// The layouts' days: an agenda of fifteen days from its anchor, one day,
    /// and the ISO week holding the anchor.
    #[test]
    fn each_layout_covers_its_days() {
        assert_eq!(
            CalendarLayout::Agenda.days(d("2026-07-08")),
            (d("2026-07-08"), d("2026-07-22"))
        );
        assert_eq!(
            CalendarLayout::Day.days(d("2026-07-08")),
            (d("2026-07-08"), d("2026-07-08"))
        );
        assert_eq!(
            CalendarLayout::Week.days(d("2026-07-08")),
            (d("2026-07-06"), d("2026-07-12"))
        );
    }
}
