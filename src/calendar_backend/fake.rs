//! A server calendar in memory, for tests (PERSO-200).
//!
//! In-process tests build one with [`FakeCalendarBackend::new`] or
//! [`FakeCalendarBackend::canned`] and read the answers back with
//! [`FakeCalendarBackend::responses`]. A daemon slice test cannot reach into
//! the daemon's memory, so the daemon arms the fake from [`FAKE_CALENDAR_ENV`]
//! instead, the way [`crate::daemon::fake_transport`] arms the SMTP fake, and
//! the test reads the answers from the log, one JSON line per `respond`:
//!
//! ```json
//! {
//!   "calendars": [<Calendar>...],
//!   "events": [<Event>...],
//!   "log": "<path>",
//!   "missing_scope": false
//! }
//! ```

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::anyhow;
use chrono::{NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use super::{BackendResult, CalendarBackend, CalendarBackendError, Response, SyncResult};
use crate::calendar_model::{
    Attendee, AttendeeRole, Calendar, CalendarWindow, Event, EventTime, OwnResponse, Person,
    ResponseStatus, Sensitivity, ShowAs,
};

/// The hook. Set on the daemon, never on a client.
pub const FAKE_CALENDAR_ENV: &str = "MAILYPOPPINS_DAEMON_FAKE_CALENDAR";

/// The scope a [`FakeCalendarBackend::missing_scope`] fake reports missing.
pub const FAKE_MISSING_SCOPE: &str = "Calendars.ReadWrite";

/// The token a fake hands out; a sync that passes it back gets no changes.
const FAKE_TOKEN: &str = "fake-calendar-token";

/// One recorded `respond` call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedResponse {
    pub event: String,
    pub response: Response,
    pub comment: Option<String>,
    pub send_response: bool,
}

/// What [`FAKE_CALENDAR_ENV`] holds.
#[derive(Deserialize)]
struct FakeSpec {
    #[serde(default)]
    calendars: Vec<Calendar>,
    #[serde(default)]
    events: Vec<Event>,
    #[serde(default)]
    log: Option<PathBuf>,
    #[serde(default)]
    missing_scope: bool,
}

/// A calendar server with fixed contents that records every answer.
#[derive(Debug, Clone, Default)]
pub struct FakeCalendarBackend {
    calendars: Vec<Calendar>,
    events: Vec<Event>,
    missing_scope: bool,
    log: Option<PathBuf>,
    responses: Arc<Mutex<Vec<RecordedResponse>>>,
}

impl FakeCalendarBackend {
    pub fn new(calendars: Vec<Calendar>, events: Vec<Event>) -> Self {
        FakeCalendarBackend {
            calendars,
            events,
            ..Default::default()
        }
    }

    /// The fake this process runs under, or `None` when the hook is unset or
    /// unparseable (an environment variable a process did not understand never
    /// stops it, the rule of every daemon hook).
    pub fn from_env() -> Option<Self> {
        let raw = std::env::var(FAKE_CALENDAR_ENV).ok()?;
        if raw.trim().is_empty() {
            return None;
        }
        let spec: FakeSpec = serde_json::from_str(&raw).ok()?;
        Some(FakeCalendarBackend {
            calendars: spec.calendars,
            events: spec.events,
            missing_scope: spec.missing_scope,
            log: spec.log,
            responses: Arc::default(),
        })
    }

    /// Every call fails with [`CalendarBackendError::MissingScope`], the way
    /// a Graph token issued before the calendar scope does.
    pub fn missing_scope(mut self) -> Self {
        self.missing_scope = true;
        self
    }

    /// Append each answer to `path` as a JSON line, besides keeping it.
    pub fn with_log(mut self, path: PathBuf) -> Self {
        self.log = Some(path);
        self
    }

    /// The answers given so far, in order.
    pub fn responses(&self) -> Vec<RecordedResponse> {
        self.responses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// One account `evoqs` with a default and a second calendar, holding a
    /// timed meeting to answer, an all-day event and one occurrence of a
    /// weekly series, all in November 2026.
    pub fn canned() -> Self {
        let calendar = |id: &str, name: &str, is_default: bool| Calendar {
            account: "evoqs".into(),
            id: id.into(),
            name: name.into(),
            color: Some("#3b82f6".into()),
            is_default,
            can_edit: true,
            visible: true,
        };
        let base = |id: &str, calendar: &str, summary: &str, time: EventTime| Event {
            account: "evoqs".into(),
            calendar: calendar.into(),
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
            own_response: OwnResponse::None,
            recurrence: None,
            reminder: None,
            online_meeting_url: None,
            sensitivity: Sensitivity::Normal,
            show_as: ShowAs::Busy,
            cancelled: false,
            is_organizer: false,
            last_modified: None,
            web_link: None,
        };
        let at = |d: u32, h: u32| Utc.with_ymd_and_hms(2026, 11, d, h, 0, 0).unwrap();
        let day = |d: u32| NaiveDate::from_ymd_opt(2026, 11, d).unwrap();

        let mut meeting = base(
            "ev-meeting",
            "cal-main",
            "Project review",
            EventTime::timed(at(3, 9), at(3, 10), "Europe/Berlin").unwrap(),
        );
        meeting.location = Some("Room 1".into());
        meeting.organizer = Some(Person {
            name: Some("Ada Lovelace".into()),
            email: "ada@example.com".into(),
        });
        meeting.attendees = vec![Attendee {
            name: Some("Sylvain".into()),
            email: "sylvain@example.com".into(),
            role: AttendeeRole::Required,
            status: ResponseStatus::NeedsAction,
        }];
        meeting.own_response = OwnResponse::NeedsAction;
        meeting.reminder = Some(15);

        let mut holiday = base(
            "ev-holiday",
            "cal-second",
            "Holiday",
            EventTime::all_day(day(5), day(7)).unwrap(),
        );
        holiday.show_as = ShowAs::Oof;

        let mut occurrence = base(
            "ev-weekly-2",
            "cal-main",
            "Weekly sync",
            EventTime::timed(at(10, 14), at(10, 15), "Europe/Berlin").unwrap(),
        );
        occurrence.uid = "ev-weekly@example.com".into();
        occurrence.recurrence_id = Some("2026-11-10T14:00:00Z".into());
        occurrence.series_master_id = Some("ev-weekly".into());
        occurrence.is_organizer = true;
        occurrence.own_response = OwnResponse::Organizer;

        FakeCalendarBackend::new(
            vec![
                calendar("cal-main", "Calendar", true),
                calendar("cal-second", "Holidays", false),
            ],
            vec![meeting, holiday, occurrence],
        )
    }

    fn check_scope(&self) -> BackendResult<()> {
        if self.missing_scope {
            return Err(CalendarBackendError::MissingScope {
                scope: FAKE_MISSING_SCOPE.into(),
            });
        }
        Ok(())
    }

    fn record(&self, recorded: RecordedResponse) -> BackendResult<()> {
        if let Some(path) = &self.log {
            let mut line = serde_json::to_string(&recorded).map_err(anyhow::Error::from)?;
            line.push('\n');
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut file| file.write_all(line.as_bytes()))
                .map_err(anyhow::Error::from)?;
        }
        self.responses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(recorded);
        Ok(())
    }
}

impl CalendarBackend for FakeCalendarBackend {
    async fn list_calendars(&self) -> BackendResult<Vec<Calendar>> {
        self.check_scope()?;
        Ok(self.calendars.clone())
    }

    /// A baseline (no token) returns the calendar's events in the window, all
    /// day ones seen from UTC; the fake's own token returns no changes; any
    /// other token is refused the way Graph answers 410, with a baseline.
    async fn sync_window(
        &self,
        calendar: &Calendar,
        window: CalendarWindow,
        token: Option<String>,
    ) -> BackendResult<SyncResult> {
        self.check_scope()?;
        if token.as_deref() == Some(FAKE_TOKEN) {
            return Ok(SyncResult {
                token,
                ..Default::default()
            });
        }
        let upserts = self
            .events
            .iter()
            .filter(|event| {
                event.calendar == calendar.id && event.overlaps(&window, chrono_tz::UTC)
            })
            .cloned()
            .collect();
        Ok(SyncResult {
            upserts,
            removed: Vec::new(),
            token: Some(FAKE_TOKEN.into()),
            full_resync: true,
        })
    }

    async fn respond(
        &self,
        event: &str,
        response: Response,
        comment: Option<String>,
        send_response: bool,
    ) -> BackendResult<()> {
        self.check_scope()?;
        if !self.events.iter().any(|known| known.id == event) {
            return Err(anyhow!("no event with id {event}").into());
        }
        self.record(RecordedResponse {
            event: event.to_string(),
            response,
            comment,
            send_response,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar_model::window_for;

    fn november() -> CalendarWindow {
        window_for(Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap())
    }

    #[tokio::test]
    async fn a_baseline_returns_the_calendars_events_and_its_token_returns_none() {
        let fake = FakeCalendarBackend::canned();
        let calendars = fake.list_calendars().await.unwrap();
        assert_eq!(calendars.len(), 2);
        let main = &calendars[0];

        let baseline = fake.sync_window(main, november(), None).await.unwrap();
        assert!(baseline.full_resync);
        let ids: Vec<&str> = baseline.upserts.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["ev-meeting", "ev-weekly-2"]);

        let delta = fake
            .sync_window(main, november(), baseline.token.clone())
            .await
            .unwrap();
        assert!(delta.upserts.is_empty() && !delta.full_resync);
        assert_eq!(delta.token, baseline.token);

        let stale = fake
            .sync_window(main, november(), Some("expired".into()))
            .await
            .unwrap();
        assert!(stale.full_resync);
        assert_eq!(stale.upserts.len(), 2);
    }

    #[tokio::test]
    async fn a_window_outside_the_events_returns_nothing() {
        let fake = FakeCalendarBackend::canned();
        let far = window_for(Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap());
        let calendars = fake.list_calendars().await.unwrap();
        let result = fake.sync_window(&calendars[1], far, None).await.unwrap();
        assert!(result.upserts.is_empty());
    }

    #[tokio::test]
    async fn respond_is_recorded_in_memory_and_in_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("respond.jsonl");
        let fake = FakeCalendarBackend::canned().with_log(log.clone());
        fake.respond(
            "ev-meeting",
            Response::Tentative,
            Some("late".into()),
            false,
        )
        .await
        .unwrap();
        let expected = RecordedResponse {
            event: "ev-meeting".into(),
            response: Response::Tentative,
            comment: Some("late".into()),
            send_response: false,
        };
        assert_eq!(fake.responses(), std::slice::from_ref(&expected));
        let line = std::fs::read_to_string(&log).unwrap();
        let logged: RecordedResponse = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(logged, expected);

        let unknown = fake.respond("nope", Response::Accept, None, true).await;
        assert!(matches!(unknown, Err(CalendarBackendError::Other(_))));
        assert_eq!(fake.responses().len(), 1);
    }

    #[tokio::test]
    async fn a_fake_without_the_scope_refuses_every_call() {
        let fake = FakeCalendarBackend::canned().missing_scope();
        let err = fake.list_calendars().await.unwrap_err();
        assert!(matches!(
            &err,
            CalendarBackendError::MissingScope { scope } if scope == FAKE_MISSING_SCOPE
        ));
        assert!(err.to_string().contains("Calendars.ReadWrite"));
        assert!(fake
            .respond("ev-meeting", Response::Accept, None, true)
            .await
            .is_err());
        assert!(fake.responses().is_empty());
    }

    #[test]
    fn the_env_spec_parses_its_calendars_and_events() {
        let canned = FakeCalendarBackend::canned();
        let spec = serde_json::json!({
            "calendars": canned.calendars,
            "events": canned.events,
            "missing_scope": true,
        });
        let parsed: FakeSpec = serde_json::from_value(spec).unwrap();
        assert_eq!(parsed.calendars, canned.calendars);
        assert_eq!(parsed.events, canned.events);
        assert!(parsed.missing_scope && parsed.log.is_none());
    }
}
