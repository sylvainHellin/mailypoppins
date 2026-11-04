//! A server calendar in memory, for tests (PERSO-200).
//!
//! In-process tests build one with [`FakeCalendarBackend::new`] or
//! [`FakeCalendarBackend::canned`] and read the answers back with
//! [`FakeCalendarBackend::responses`]. A daemon slice test cannot reach into
//! the daemon's memory, so the daemon arms the fake from [`FAKE_CALENDAR_ENV`]
//! instead, the way [`crate::daemon::fake_transport`] arms the SMTP fake, and
//! the test reads the answers from the log, one JSON line per `respond`, and
//! the writes from the write log, one [`RecordedWrite`] line per create,
//! update, cancel, delete, dismiss or snooze:
//!
//! ```json
//! {
//!   "calendars": [<Calendar>...],
//!   "events": [<Event>...],
//!   "log": "<path>",
//!   "write_log": "<path>",
//!   "missing_scope": false,
//!   "message_events": {"<server message id>": "<event id>"},
//!   "features": ["write", ...],
//!   "series": [<SeriesInfo>...],
//!   "free_busy": {"<address>": [<ScheduleItem>...]}
//! }
//! ```
//!
//! `free_busy` is what [`CalendarBackend::free_busy`] answers: each address
//! it names has those busy blocks, its view computed from them, and any other
//! address has no free/busy ([`FAKE_NO_FREE_BUSY`]), as Graph answers for an
//! address outside the tenant. Addresses match whatever their case.
//!
//! A spec calendar offers Teams when its `online_meeting_providers` names
//! `teams-for-business`, as the test fixtures' calendars do unless a test
//! clears it; `calendar.list` then reports the spec's `online-meeting`
//! feature on it ([`super::calendar_features`]).
//!
//! The writes change the fake's events the way a server does: a create adds
//! one (or answers the one an earlier create with the same transaction id
//! made, patched with the create's fields when they changed, as the Graph
//! backend does with Graph's answer), an update patches it, a cancel or a
//! delete removes it, and the next incremental sync reports each change.
//!
//! A series behaves as Graph's does (`.agents/research/
//! 2026-11-04-graph-recurrence-probes.md`): a create with a rule makes a
//! master, kept with the series (`series` in the spec), and expands its
//! occurrences, each with an id of the master's plus its date, the master
//! moved to the first of them; a write to the master's id answers the
//! series, a new time or rule expanding the occurrences afresh (an exception
//! reset, an occurrence the rule keeps keeping its id) and any other change
//! reaching the occurrences that were not changed apart; a write to an
//! occurrence makes it an exception. A sync after a change to a series
//! reports its master and every occurrence again, and never an occurrence
//! that left it: a cancelled or deleted occurrence, or one the new rule
//! dropped. A cancelled or deleted series reports its master removed only.
//!
//! The daemon serves every account whose `calendar_backend` is not `none`
//! from the fake, through [`FakeCalendarBackend::armed`]: one server per
//! account, built from the spec once and kept for the life of the process,
//! so what a write changed is still there at the next poll. Its calendars'
//! and events' `account` is that account's name ([`FakeCalendarBackend::serving`]).

use std::collections::HashMap;

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::anyhow;
use chrono::{DateTime, Days, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use super::{
    BackendResult, CalendarBackend, CalendarBackendError, Response, SeriesInfo, SyncResult, Written,
};
use crate::calendar_freebusy::{FreeBusyQuery, Schedule, ScheduleItem};
use crate::calendar_model::{
    Attendee, AttendeeRole, Calendar, CalendarFeature, CalendarWindow, Event, EventKind, EventTime,
    Frequency, OnlineMeetingProvider, OwnResponse, Person, Recurrence, ResponseStatus, Sensitivity,
    ShowAs, WeekIndex, Weekday,
};
use crate::calendar_write::{AttendeeInput, EventInput, EventPatch, ProposedTime};

/// The reminder a created event gets when its input leaves it to the
/// mailbox default, Outlook's own default.
pub const FAKE_DEFAULT_REMINDER: u32 = 15;

/// The hook. Set on the daemon, never on a client.
pub const FAKE_CALENDAR_ENV: &str = "MAILYPOPPINS_DAEMON_FAKE_CALENDAR";

/// The scope a [`FakeCalendarBackend::missing_scope`] fake reports missing.
pub const FAKE_MISSING_SCOPE: &str = "Calendars.ReadWrite";

/// Why the fake has no free/busy for an address its spec does not name, in
/// the words Graph's own error for an address outside the tenant reads as.
pub const FAKE_NO_FREE_BUSY: &str = "Proxy web request failed (5016)";

/// The prefix of the tokens a fake hands out: `fake-calendar-token:<e>:<n>`,
/// where `e` counts the [`FakeCalendarBackend::expire_tokens`] calls and `n`
/// the changes made to the fake so far. A sync that passes one of the current
/// `e` back gets the changes made since; any other token is refused the way
/// Graph answers 410.
const FAKE_TOKEN_PREFIX: &str = "fake-calendar-token:";

/// The server side of a fake: its events and the log of what changed.
#[derive(Debug, Default)]
struct FakeState {
    events: Vec<Event>,
    /// The id of each event put or removed, in order; the index plus one is
    /// the generation the change made.
    changes: Vec<String>,
    /// Bumped by [`FakeCalendarBackend::expire_tokens`]; a token of an
    /// earlier epoch is refused, as a server's expired sync state.
    epoch: u64,
    /// The event each create's transaction id made and the input it was
    /// last written with, as Graph keeps the `transactionId` to answer a
    /// repeated create with the same event.
    transactions: HashMap<String, (String, EventInput)>,
    /// How many events the creates made, for their ids.
    created: u64,
    /// The busy blocks [`CalendarBackend::free_busy`] answers, by address
    /// lowercased.
    free_busy: HashMap<String, Vec<ScheduleItem>>,
    /// The series the fake holds, their masters with them: what a baseline
    /// reports resolved, as Graph's does ([`SyncResult::series`]), so the
    /// store's series cache fills.
    series: Vec<SeriesInfo>,
}

impl FakeState {
    fn generation(&self) -> u64 {
        self.changes.len() as u64
    }
}

/// One recorded `respond` call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedResponse {
    pub event: String,
    pub response: Response,
    pub comment: Option<String>,
    pub send_response: bool,
    /// The new time the answer proposed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_time: Option<ProposedTime>,
}

/// One recorded write, in the order the calls came.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "call", rename_all = "snake_case")]
pub enum RecordedWrite {
    Create {
        calendar: String,
        transaction_id: String,
        input: EventInput,
        /// The id of the event the create made or found.
        event: String,
    },
    Update {
        calendar: String,
        event: String,
        patch: EventPatch,
    },
    Cancel {
        event: String,
        comment: Option<String>,
    },
    Delete {
        event: String,
    },
    DismissReminder {
        event: String,
    },
    SnoozeReminder {
        event: String,
        until: DateTime<Utc>,
    },
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
    write_log: Option<PathBuf>,
    #[serde(default)]
    missing_scope: bool,
    #[serde(default)]
    message_events: HashMap<String, String>,
    #[serde(default)]
    features: Vec<CalendarFeature>,
    #[serde(default)]
    series: Vec<SeriesInfo>,
    #[serde(default)]
    free_busy: HashMap<String, Vec<ScheduleItem>>,
}

/// A calendar server with fixed contents that records every answer.
///
/// A clone shares the server side, so a test keeps one handle to change the
/// events ([`FakeCalendarBackend::put_event`], [`FakeCalendarBackend::remove_event`])
/// while the engine syncs through another.
#[derive(Debug, Clone, Default)]
pub struct FakeCalendarBackend {
    calendars: Vec<Calendar>,
    state: Arc<Mutex<FakeState>>,
    missing_scope: bool,
    log: Option<PathBuf>,
    write_log: Option<PathBuf>,
    responses: Arc<Mutex<Vec<RecordedResponse>>>,
    writes: Arc<Mutex<Vec<RecordedWrite>>>,
    /// The invitation mails the fake knows the event of, by server message
    /// id ([`CalendarBackend::event_for_message`]).
    message_events: HashMap<String, String>,
    /// What the fake claims to serve ([`super::features`]): the spec's
    /// `features`, none unless a test names some.
    features: Vec<CalendarFeature>,
}

impl FakeCalendarBackend {
    pub fn new(calendars: Vec<Calendar>, events: Vec<Event>) -> Self {
        FakeCalendarBackend {
            calendars,
            state: Arc::new(Mutex::new(FakeState {
                events,
                ..Default::default()
            })),
            ..Default::default()
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Add `event`, or replace the one with its id, as a server-side change
    /// the next incremental sync reports.
    pub fn put_event(&self, event: Event) {
        let mut state = self.state();
        state.events.retain(|known| known.id != event.id);
        state.changes.push(event.id.clone());
        state.events.push(event);
    }

    /// Delete the event with `id`, as a server-side change.
    pub fn remove_event(&self, id: &str) {
        let mut state = self.state();
        state.events.retain(|known| known.id != id);
        state.changes.push(id.to_string());
    }

    /// Delete the event with `id` as Outlook does, as a server-side change:
    /// a series master with all its occurrences, the next sync naming the
    /// master alone; one occurrence silently, the next sync re-sending its
    /// series without it.
    pub fn delete_in_outlook(&self, id: &str) {
        self.state().take_off(id);
    }

    /// Refuse every token handed out so far, as a server whose sync state
    /// expired (Graph's 410): the next sync is a fresh baseline.
    pub fn expire_tokens(&self) {
        self.state().epoch += 1;
    }

    /// The events the fake holds now.
    pub fn events(&self) -> Vec<Event> {
        self.state().events.clone()
    }

    /// The fake serving `account` in this process, or `None` when the hook
    /// is unset or unparseable (an environment variable a process did not
    /// understand never stops it, the rule of every daemon hook).
    ///
    /// The first call for an account builds its server from the spec
    /// ([`Self::from_env`], [`Self::serving`]); every later one answers a
    /// clone of it, which shares its events, so a write survives into the
    /// next sync as it would on a real server.
    pub fn armed(account: &str) -> Option<Self> {
        static SERVERS: OnceLock<Mutex<HashMap<(String, String), FakeCalendarBackend>>> =
            OnceLock::new();
        let raw = std::env::var(FAKE_CALENDAR_ENV).ok()?;
        let mut servers = SERVERS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let key = (raw, account.to_string());
        if let Some(fake) = servers.get(&key) {
            return Some(fake.clone());
        }
        let fake = Self::from_env()?.serving(account);
        servers.insert(key, fake.clone());
        Some(fake)
    }

    /// A fresh fake from the spec this process runs under, or `None` when
    /// the hook is unset or unparseable.
    pub fn from_env() -> Option<Self> {
        let raw = std::env::var(FAKE_CALENDAR_ENV).ok()?;
        if raw.trim().is_empty() {
            return None;
        }
        let spec: FakeSpec = serde_json::from_str(&raw).ok()?;
        let mut fake = FakeCalendarBackend::new(spec.calendars, spec.events);
        fake.missing_scope = spec.missing_scope;
        fake.log = spec.log;
        fake.write_log = spec.write_log;
        fake.message_events = spec.message_events;
        fake.features = spec.features;
        fake.state().series = spec.series;
        fake.state().free_busy = spec
            .free_busy
            .into_iter()
            .map(|(address, items)| (address.to_ascii_lowercase(), items))
            .collect();
        Some(fake)
    }

    /// The same server, serving `account`: every calendar and event names it,
    /// whatever account the spec wrote, so one armed spec serves every
    /// account with a backend.
    pub fn serving(mut self, account: &str) -> Self {
        for calendar in &mut self.calendars {
            calendar.account = account.to_string();
        }
        for event in &mut self.state().events {
            event.account = account.to_string();
        }
        self
    }

    /// Answer [`CalendarBackend::event_for_message`] for `message_id` with
    /// `event`.
    pub fn with_message_event(mut self, message_id: &str, event: &str) -> Self {
        self.message_events
            .insert(message_id.to_string(), event.to_string());
        self
    }

    /// Every call fails with [`CalendarBackendError::MissingScope`], the way
    /// a Graph token issued before the calendar scope does.
    pub fn missing_scope(mut self) -> Self {
        self.missing_scope = true;
        self
    }

    /// What the fake claims to serve ([`super::features`]), as its spec
    /// names it.
    pub fn features(&self) -> Vec<CalendarFeature> {
        self.features.clone()
    }

    /// Answer `items` as `address`'s busy blocks in a free/busy query.
    pub fn with_free_busy(self, address: &str, items: Vec<ScheduleItem>) -> Self {
        self.state()
            .free_busy
            .insert(address.to_ascii_lowercase(), items);
        self
    }

    /// Hold `series`, which every baseline reports resolved and a write to
    /// a master's id reaches.
    pub fn with_series(self, series: Vec<SeriesInfo>) -> Self {
        self.state().series = series;
        self
    }

    /// The series the fake holds now.
    pub fn series(&self) -> Vec<SeriesInfo> {
        self.state().series.clone()
    }

    /// Append each answer to `path` as a JSON line, besides keeping it.
    pub fn with_log(mut self, path: PathBuf) -> Self {
        self.log = Some(path);
        self
    }

    /// Append each write to `path` as a JSON line, besides keeping it.
    pub fn with_write_log(mut self, path: PathBuf) -> Self {
        self.write_log = Some(path);
        self
    }

    /// The answers given so far, in order.
    pub fn responses(&self) -> Vec<RecordedResponse> {
        self.responses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// The writes made so far, in order.
    pub fn writes(&self) -> Vec<RecordedWrite> {
        self.writes
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
            online_meeting_providers: vec![OnlineMeetingProvider::TeamsForBusiness],
        };
        let base = |id: &str, calendar: &str, summary: &str, time: EventTime| Event {
            account: "evoqs".into(),
            calendar: calendar.into(),
            id: id.into(),
            uid: format!("{id}@example.com"),
            recurrence_id: None,
            series_master_id: None,
            kind: EventKind::Single,
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
            is_online_meeting: false,
            online_meeting_provider: None,
            sensitivity: Sensitivity::Normal,
            show_as: ShowAs::Busy,
            cancelled: false,
            is_organizer: false,
            last_modified: None,
            web_link: None,
            allow_new_time_proposals: true,
            response_requested: true,
            transaction_id: None,
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
            proposed_time: None,
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
        occurrence.kind = EventKind::Occurrence;
        occurrence.recurrence = Some("Weekly on Tuesday".into());
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

    /// [`CalendarBackendError::MissingScope`] for a fake armed without the
    /// scope, the check a write runs before its operation starts.
    pub fn check_scope(&self) -> BackendResult<()> {
        if self.missing_scope {
            return Err(CalendarBackendError::MissingScope {
                scope: FAKE_MISSING_SCOPE.into(),
            });
        }
        Ok(())
    }

    fn record(&self, recorded: RecordedResponse) -> BackendResult<()> {
        append_line(self.log.as_ref(), &recorded)?;
        self.responses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(recorded);
        Ok(())
    }

    fn record_write(&self, recorded: RecordedWrite) -> BackendResult<()> {
        append_line(self.write_log.as_ref(), &recorded)?;
        self.writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(recorded);
        Ok(())
    }
}

/// Append `value` to the log at `path`, if any, as one JSON line.
fn append_line(path: Option<&PathBuf>, value: &impl Serialize) -> BackendResult<()> {
    let Some(path) = path else {
        return Ok(());
    };
    let mut line = serde_json::to_string(value).map_err(anyhow::Error::from)?;
    line.push('\n');
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(line.as_bytes()))
        .map_err(anyhow::Error::from)?;
    Ok(())
}

/// An attendee of a written event: one already invited keeps their answer,
/// a new one has not answered yet.
fn written_attendee(input: &AttendeeInput, before: &[Attendee]) -> Attendee {
    let status = before
        .iter()
        .find(|known| known.email.eq_ignore_ascii_case(&input.email))
        .map_or(ResponseStatus::NeedsAction, |known| known.status);
    Attendee {
        name: input.name.clone(),
        email: input.email.clone(),
        role: input.role,
        status,
        proposed_time: None,
    }
}

/// The join link the fake provisions for an online meeting.
fn join_link(id: &str) -> String {
    format!("https://teams.example.com/join/{id}")
}

/// Turn `event` into an online meeting with the fake's join link, as Graph
/// provisions a Teams meeting.
fn provision_meeting(event: &mut Event) {
    event.is_online_meeting = true;
    event.online_meeting_provider = Some(OnlineMeetingProvider::TeamsForBusiness);
    event.online_meeting_url = Some(join_link(&event.id));
}

/// The event a create of `input` makes in `calendar` under `id`, the user
/// its organizer, carrying the create's `transaction_id` as Graph's does.
fn created_event(calendar: &Calendar, id: &str, input: &EventInput, transaction_id: &str) -> Event {
    let mut event = Event {
        account: calendar.account.clone(),
        calendar: calendar.id.clone(),
        id: id.to_string(),
        uid: format!("{id}@fake.example.com"),
        recurrence_id: None,
        series_master_id: None,
        kind: EventKind::Single,
        summary: input.summary.clone(),
        time: input.time.clone(),
        location: input.location.clone(),
        body: input.body.clone(),
        organizer: None,
        attendees: input
            .attendees
            .iter()
            .map(|a| written_attendee(a, &[]))
            .collect(),
        own_response: OwnResponse::Organizer,
        recurrence: None,
        reminder: input.reminder.unwrap_or(Some(FAKE_DEFAULT_REMINDER)),
        online_meeting_url: None,
        is_online_meeting: false,
        online_meeting_provider: None,
        sensitivity: input.sensitivity.unwrap_or(Sensitivity::Normal),
        show_as: input.show_as.unwrap_or(ShowAs::Busy),
        cancelled: false,
        is_organizer: true,
        last_modified: None,
        web_link: None,
        allow_new_time_proposals: input.allow_new_time_proposals.unwrap_or(true),
        response_requested: true,
        transaction_id: Some(transaction_id.to_string()).filter(|t| !t.trim().is_empty()),
    };
    if input.online_meeting {
        provision_meeting(&mut event);
    }
    event
}

/// The most occurrences the fake expands a series into.
const FAKE_MAX_OCCURRENCES: usize = 60;

/// How far past its first day the fake expands a series without end.
const FAKE_HORIZON_DAYS: u64 = 3 * 366;

/// Whether `date` is a day `rule` places when the series starts on `first`.
fn on_rule(rule: &Recurrence, first: NaiveDate, date: NaiveDate) -> bool {
    use chrono::Datelike;
    let interval = i64::from(rule.interval.max(1));
    let weekday = Weekday::of_date(date);
    let on_day = || match rule.week_index {
        None => Some(date.day()) == rule.day_of_month,
        Some(index) => {
            rule.weekdays.contains(&weekday)
                && match index {
                    WeekIndex::Last => (date + Days::new(7)).month() != date.month(),
                    _ => (date.day() - 1) / 7 + 1 == index.position() as u32,
                }
        }
    };
    let months = |d: NaiveDate| i64::from(d.year()) * 12 + i64::from(d.month0());
    match rule.frequency {
        Frequency::Daily => (date - first).num_days() % interval == 0,
        Frequency::Weekly => {
            let start_of_week = |d: NaiveDate| {
                let from = rule.first_day_of_week.unwrap_or(Weekday::Monday);
                let offset = (Weekday::ALL
                    .iter()
                    .position(|w| *w == Weekday::of_date(d))
                    .unwrap()
                    + 7
                    - Weekday::ALL.iter().position(|w| *w == from).unwrap())
                    % 7;
                d - Days::new(offset as u64)
            };
            let weeks = (start_of_week(date) - start_of_week(first)).num_days() / 7;
            rule.weekdays.contains(&weekday) && weeks % interval == 0
        }
        Frequency::Monthly => (months(date) - months(first)) % interval == 0 && on_day(),
        Frequency::Yearly => {
            i64::from(date.year() - first.year()) % interval == 0
                && Some(date.month()) == rule.month
                && on_day()
        }
    }
}

/// The days `rule` places from `first` on, within its end and the fake's
/// bounds.
fn occurrence_dates(rule: &Recurrence, first: NaiveDate) -> Vec<NaiveDate> {
    let horizon = first + Days::new(FAKE_HORIZON_DAYS);
    let last = rule.until.map_or(horizon, |until| until.min(horizon));
    let limit = rule
        .count
        .map_or(FAKE_MAX_OCCURRENCES, |count| count as usize)
        .min(FAKE_MAX_OCCURRENCES);
    let mut dates = Vec::new();
    let mut date = first;
    while date <= last && dates.len() < limit {
        if on_rule(rule, first, date) {
            dates.push(date);
        }
        date = date + Days::new(1);
    }
    dates
}

/// `time` moved to `date`, keeping its wall clock in its zone and its
/// length.
fn time_on(time: &EventTime, date: NaiveDate) -> EventTime {
    match time {
        EventTime::Timed { start, end, zone } => {
            let tz: chrono_tz::Tz = zone.parse().unwrap_or(chrono_tz::UTC);
            let clock = start.with_timezone(&tz).time();
            let moved = tz
                .from_local_datetime(&date.and_time(clock))
                .earliest()
                .map_or(*start, |at| at.with_timezone(&Utc));
            EventTime::Timed {
                start: moved,
                end: moved + (*end - *start),
                zone: zone.clone(),
            }
        }
        EventTime::AllDay { start, end } => EventTime::AllDay {
            start: date,
            end: date + (*end - *start),
        },
    }
}

/// The occurrences of the series `master` repeats by `rule`, ids of the
/// master's plus their date, with the master moved to the first of them.
fn expand(master: &mut Event, rule: &Recurrence) -> Vec<Event> {
    let zone = master.time.zone().unwrap_or(chrono_tz::UTC);
    let first = crate::calendar_write::first_day(&master.time, zone);
    let dates = occurrence_dates(rule, rule.start.unwrap_or(first).max(first));
    if let Some(date) = dates.first() {
        master.time = time_on(&master.time, *date);
    }
    dates
        .into_iter()
        .map(|date| {
            let mut occurrence = master.clone();
            occurrence.id = format!("{}-{}", master.id, date.format("%Y%m%d"));
            occurrence.kind = EventKind::Occurrence;
            occurrence.series_master_id = Some(master.id.clone());
            occurrence.time = time_on(&master.time, date);
            occurrence.recurrence_id = Some(match &occurrence.time {
                EventTime::Timed { start, .. } => {
                    start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                }
                EventTime::AllDay { start, .. } => start.to_string(),
            });
            occurrence
        })
        .collect()
}

/// `event` with the fields `patch` names changed.
fn apply_patch(event: &mut Event, patch: &EventPatch) {
    if let Some(summary) = &patch.summary {
        event.summary = summary.clone();
    }
    if let Some(time) = &patch.time {
        event.time = time.clone();
    }
    if let Some(location) = &patch.location {
        event.location = location.clone();
    }
    if let Some(body) = &patch.body {
        event.body = body.clone();
    }
    if let Some(attendees) = &patch.attendees {
        event.attendees = attendees
            .iter()
            .map(|a| written_attendee(a, &event.attendees))
            .collect();
    }
    // Once on, the link stays, as on Graph, which ignores turning it off.
    if patch.online_meeting == Some(true) && event.online_meeting_url.is_none() {
        provision_meeting(event);
    }
    if let Some(reminder) = patch.reminder {
        event.reminder = reminder;
    }
    if let Some(show_as) = patch.show_as {
        event.show_as = show_as;
    }
    if let Some(sensitivity) = patch.sensitivity {
        event.sensitivity = sensitivity;
    }
    if let Some(allow) = patch.allow_new_time_proposals {
        event.allow_new_time_proposals = allow;
    }
    if let Some(rule) = &patch.recurrence {
        event.recurrence = Some(rule.describe());
    }
}

impl FakeState {
    /// The series whose master is `id`.
    fn series_of(&self, id: &str) -> Option<usize> {
        self.series.iter().position(|series| series.master_id == id)
    }

    /// Replace the occurrences of the series at `at` with the ones its rule
    /// places now, the master moved to the first of them.
    fn expand_series(&mut self, at: usize) {
        let series = &mut self.series[at];
        let (Some(rule), Some(master)) = (series.recurrence.clone(), series.master.as_mut()) else {
            return;
        };
        let occurrences = expand(master, &rule);
        let id = series.master_id.clone();
        self.events
            .retain(|event| event.series_master_id.as_deref() != Some(id.as_str()));
        self.events.extend(occurrences);
    }

    /// Patch the series at `at` through its master: a new time or rule
    /// expands it afresh, any other change reaches the occurrences not
    /// changed apart. Recorded as a change of the master, which a sync
    /// answers with the series and its occurrences.
    fn patch_series(&mut self, at: usize, patch: &EventPatch) -> BackendResult<SeriesInfo> {
        let series = &mut self.series[at];
        let master = series
            .master
            .as_mut()
            .ok_or_else(|| anyhow!("the fake holds no master for {}", series.master_id))?;
        apply_patch(master, patch);
        if let Some(rule) = &patch.recurrence {
            series.recurrence = Some(rule.clone());
        }
        let id = series.master_id.clone();
        if patch.time.is_some() || patch.recurrence.is_some() {
            self.expand_series(at);
        } else {
            let master = self.series[at].master.clone().expect("checked above");
            for event in &mut self.events {
                if event.series_master_id.as_deref() == Some(id.as_str())
                    && event.kind == EventKind::Occurrence
                {
                    event.inherit_from(&master);
                }
            }
        }
        self.changes.push(id);
        Ok(self.series[at].clone())
    }

    /// Take the event `id` off the server, as a cancel or a delete does: a
    /// series with all its occurrences, recorded as its master removed; one
    /// occurrence silently, its series recorded as changed, as Graph's delta
    /// re-sends the series and never names the occurrence; any other event
    /// as removed.
    fn take_off(&mut self, id: &str) {
        if let Some(at) = self.series_of(id) {
            self.series.remove(at);
            self.events
                .retain(|event| event.series_master_id.as_deref() != Some(id));
            self.changes.push(id.to_string());
            return;
        }
        let master = self
            .events
            .iter()
            .find(|event| event.id == id)
            .and_then(|event| event.series_master_id.clone());
        self.events.retain(|event| event.id != id);
        match master {
            Some(master) => self.changes.push(master),
            None => self.changes.push(id.to_string()),
        }
    }
}

impl CalendarBackend for FakeCalendarBackend {
    async fn list_calendars(&self) -> BackendResult<Vec<Calendar>> {
        self.check_scope()?;
        Ok(self.calendars.clone())
    }

    /// A baseline (no token) returns the calendar's events in the window, all
    /// day ones seen from UTC; one of the fake's own tokens returns what
    /// changed since it was handed out, an event changed out of the calendar
    /// or the window as removed and a changed series whole, as re-sent
    /// ([`SyncResult::resent_series`]); any other token, or one from before
    /// [`FakeCalendarBackend::expire_tokens`], is refused the way Graph
    /// answers 410, with a baseline.
    async fn sync_window(
        &self,
        calendar: &Calendar,
        window: CalendarWindow,
        token: Option<String>,
    ) -> BackendResult<SyncResult> {
        self.check_scope()?;
        let state = self.state();
        let belongs = |event: &Event| {
            event.calendar == calendar.id && event.overlaps(&window, chrono_tz::UTC)
        };
        let next_token = Some(format!(
            "{FAKE_TOKEN_PREFIX}{}:{}",
            state.epoch,
            state.generation()
        ));
        let since = token
            .as_deref()
            .and_then(|token| token.strip_prefix(FAKE_TOKEN_PREFIX))
            .and_then(|rest| rest.split_once(':'))
            .filter(|(epoch, _)| epoch.parse::<u64>().ok() == Some(state.epoch))
            .and_then(|(_, generation)| generation.parse::<u64>().ok())
            .filter(|generation| *generation <= state.generation());
        let Some(since) = since else {
            return Ok(SyncResult {
                upserts: state
                    .events
                    .iter()
                    .filter(|e| belongs(e))
                    .cloned()
                    .collect(),
                token: next_token,
                full_resync: true,
                path: Some("fake".into()),
                series: state.series.clone(),
                ..Default::default()
            });
        };
        let mut changed: Vec<&str> = Vec::new();
        for id in &state.changes[since as usize..] {
            if !changed.contains(&id.as_str()) {
                changed.push(id);
            }
        }
        let mut result = SyncResult {
            token: next_token,
            path: Some("fake".into()),
            ..Default::default()
        };
        for id in changed {
            if let Some(at) = state.series_of(id) {
                result.series.push(state.series[at].clone());
                result.resent_series.push(id.to_string());
                for event in &state.events {
                    if event.series_master_id.as_deref() == Some(id)
                        && belongs(event)
                        && !result.upserts.iter().any(|known| known.id == event.id)
                    {
                        result.upserts.push(event.clone());
                    }
                }
                continue;
            }
            match state.events.iter().find(|e| e.id == id && belongs(e)) {
                Some(event) => {
                    if !result.upserts.iter().any(|known| known.id == event.id) {
                        result.upserts.push(event.clone());
                    }
                }
                None => result.removed.push(id.to_string()),
            }
        }
        Ok(result)
    }

    async fn respond(
        &self,
        event: &str,
        response: Response,
        comment: Option<String>,
        send_response: bool,
        proposed: Option<ProposedTime>,
    ) -> BackendResult<()> {
        self.check_scope()?;
        if !self.state().events.iter().any(|known| known.id == event) {
            return Err(anyhow!("no event with id {event}").into());
        }
        self.record(RecordedResponse {
            event: event.to_string(),
            response,
            comment,
            send_response,
            proposed_time: proposed,
        })
    }

    /// A read-only calendar refuses the create, as a server does; a create
    /// whose transaction id made an event already answers with that event,
    /// patched with this create's fields when they changed since.
    async fn create_event(
        &self,
        calendar: &Calendar,
        input: &EventInput,
        transaction_id: &str,
    ) -> BackendResult<Written> {
        self.check_scope()?;
        if !calendar.can_edit {
            return Err(anyhow!("the calendar {} is read-only", calendar.name).into());
        }
        let series = {
            let mut state = self.state();
            let made = state
                .transactions
                .get(transaction_id)
                .and_then(|(id, last)| Some((state.series_of(id)?, last == input)));
            match made {
                Some((at, true)) => Some(state.series[at].clone()),
                Some((at, false)) => {
                    let series = state.patch_series(at, &input.as_patch())?;
                    state.transactions.insert(
                        transaction_id.to_string(),
                        (series.master_id.clone(), input.clone()),
                    );
                    Some(series)
                }
                None if input.recurrence.is_some() => {
                    state.created += 1;
                    let id = format!("fake-created-{}", state.created);
                    let mut master = created_event(calendar, &id, input, transaction_id);
                    master.kind = EventKind::SeriesMaster;
                    let rule = input.recurrence.clone().expect("checked above");
                    master.recurrence = Some(rule.describe());
                    let zone = master.time.zone().map(|tz| tz.name().to_string());
                    state.series.push(SeriesInfo {
                        master_id: id.clone(),
                        uid: master.uid.clone(),
                        recurrence: Some(rule),
                        master: Some(Box::new(master)),
                        zone: zone.or_else(|| input.zone.clone()),
                    });
                    let at = state.series.len() - 1;
                    state.expand_series(at);
                    state
                        .transactions
                        .insert(transaction_id.to_string(), (id.clone(), input.clone()));
                    state.changes.push(id);
                    Some(state.series[at].clone())
                }
                None => None,
            }
        };
        if let Some(series) = series {
            self.record_write(RecordedWrite::Create {
                calendar: calendar.id.clone(),
                transaction_id: transaction_id.to_string(),
                input: input.clone(),
                event: series.master_id.clone(),
            })?;
            return Ok(Written::Series(Box::new(series)));
        }
        let event = {
            let mut state = self.state();
            let earlier = state
                .transactions
                .get(transaction_id)
                .and_then(|(id, last)| {
                    let event = state.events.iter().find(|e| &e.id == id)?;
                    Some((event.clone(), last == input))
                });
            match earlier {
                Some((event, true)) => event,
                Some((mut event, false)) => {
                    apply_patch(&mut event, &input.as_patch());
                    state.events.retain(|known| known.id != event.id);
                    state.changes.push(event.id.clone());
                    state.events.push(event.clone());
                    state.transactions.insert(
                        transaction_id.to_string(),
                        (event.id.clone(), input.clone()),
                    );
                    event
                }
                None => {
                    state.created += 1;
                    let id = format!("fake-created-{}", state.created);
                    let event = created_event(calendar, &id, input, transaction_id);
                    state
                        .transactions
                        .insert(transaction_id.to_string(), (id.clone(), input.clone()));
                    state.changes.push(id);
                    state.events.push(event.clone());
                    event
                }
            }
        };
        self.record_write(RecordedWrite::Create {
            calendar: calendar.id.clone(),
            transaction_id: transaction_id.to_string(),
            input: input.clone(),
            event: event.id.clone(),
        })?;
        Ok(Written::Event(Box::new(event)))
    }

    async fn update_event(
        &self,
        calendar: &str,
        event: &str,
        patch: &EventPatch,
    ) -> BackendResult<Written> {
        self.check_scope()?;
        let series = {
            let mut state = self.state();
            match state.series_of(event) {
                Some(at) => Some(state.patch_series(at, patch)?),
                None => None,
            }
        };
        if let Some(series) = series {
            self.record_write(RecordedWrite::Update {
                calendar: calendar.to_string(),
                event: event.to_string(),
                patch: patch.clone(),
            })?;
            return Ok(Written::Series(Box::new(series)));
        }
        let mut updated = self
            .state()
            .events
            .iter()
            .find(|known| known.id == event)
            .cloned()
            .ok_or_else(|| anyhow!("no event with id {event}"))?;
        apply_patch(&mut updated, patch);
        if updated.kind == EventKind::Occurrence {
            updated.kind = EventKind::Exception;
        }
        self.put_event(updated.clone());
        self.record_write(RecordedWrite::Update {
            calendar: calendar.to_string(),
            event: event.to_string(),
            patch: patch.clone(),
        })?;
        Ok(Written::Event(Box::new(updated)))
    }

    /// Only the organizer cancels, as Graph answers 400 to anyone else; the
    /// cancelled event leaves the calendar.
    async fn cancel_event(&self, event: &str, comment: Option<String>) -> BackendResult<()> {
        self.check_scope()?;
        {
            let mut state = self.state();
            let organizer = match state.series_of(event) {
                Some(at) => state.series[at].master.as_ref().map(|m| m.is_organizer),
                None => state
                    .events
                    .iter()
                    .find(|known| known.id == event)
                    .map(|known| known.is_organizer),
            };
            if organizer == Some(false) {
                return Err(anyhow!("only the organizer can cancel {event}").into());
            }
            if organizer.is_some() {
                state.take_off(event);
            }
        }
        self.record_write(RecordedWrite::Cancel {
            event: event.to_string(),
            comment,
        })
    }

    async fn delete_event(&self, event: &str) -> BackendResult<()> {
        self.check_scope()?;
        {
            let mut state = self.state();
            if state.series_of(event).is_some() || state.events.iter().any(|e| e.id == event) {
                state.take_off(event);
            }
        }
        self.record_write(RecordedWrite::Delete {
            event: event.to_string(),
        })
    }

    async fn dismiss_reminder(&self, event: &str) -> BackendResult<()> {
        self.check_scope()?;
        self.record_write(RecordedWrite::DismissReminder {
            event: event.to_string(),
        })
    }

    async fn snooze_reminder(&self, event: &str, until: DateTime<Utc>) -> BackendResult<()> {
        self.check_scope()?;
        self.record_write(RecordedWrite::SnoozeReminder {
            event: event.to_string(),
            until,
        })
    }

    async fn event_for_message(&self, message_id: &str) -> BackendResult<Option<String>> {
        self.check_scope()?;
        Ok(self.message_events.get(message_id).cloned())
    }

    /// The spec's blocks of each address it names, the others without
    /// free/busy.
    async fn free_busy(&self, query: &FreeBusyQuery) -> BackendResult<Vec<Schedule>> {
        self.check_scope()?;
        let state = self.state();
        Ok(query
            .addresses
            .iter()
            .map(
                |address| match state.free_busy.get(&address.to_ascii_lowercase()) {
                    Some(items) => Schedule::from_items(address, items.clone(), query),
                    None => Schedule::unavailable(address, FAKE_NO_FREE_BUSY),
                },
            )
            .collect())
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
    async fn a_baseline_reports_the_series_it_was_given_and_a_delta_none() {
        let series = SeriesInfo {
            master_id: "ev-weekly".into(),
            uid: "ev-weekly@example.com".into(),
            recurrence: None,
            master: None,
            zone: Some("Europe/Berlin".into()),
        };
        let fake = FakeCalendarBackend::canned().with_series(vec![series.clone()]);
        let main = fake.list_calendars().await.unwrap().remove(0);
        let baseline = fake.sync_window(&main, november(), None).await.unwrap();
        assert_eq!(baseline.series, [series]);
        let delta = fake
            .sync_window(&main, november(), baseline.token)
            .await
            .unwrap();
        assert!(delta.series.is_empty());
    }

    #[tokio::test]
    async fn a_token_returns_the_changes_since_and_an_expired_one_a_baseline() {
        let fake = FakeCalendarBackend::canned();
        let main = fake.list_calendars().await.unwrap().remove(0);
        let baseline = fake.sync_window(&main, november(), None).await.unwrap();

        let mut moved = fake.events()[0].clone();
        moved.summary = "Project review, moved".into();
        fake.put_event(moved.clone());
        fake.remove_event("ev-weekly-2");
        let delta = fake
            .sync_window(&main, november(), baseline.token.clone())
            .await
            .unwrap();
        assert!(!delta.full_resync);
        assert_eq!(delta.upserts, [moved]);
        assert_eq!(delta.removed, ["ev-weekly-2"]);
        assert_ne!(delta.token, baseline.token);

        fake.expire_tokens();
        let expired = fake
            .sync_window(&main, november(), delta.token.clone())
            .await
            .unwrap();
        assert!(expired.full_resync);
        assert_eq!(expired.upserts.len(), 1);
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
        let proposed = ProposedTime {
            slot: crate::calendar_model::TimeSlot {
                start: Utc.with_ymd_and_hms(2026, 11, 18, 14, 0, 0).unwrap(),
                end: Utc.with_ymd_and_hms(2026, 11, 18, 15, 0, 0).unwrap(),
            },
            zone: "Europe/Berlin".into(),
        };
        fake.respond(
            "ev-meeting",
            Response::Tentative,
            Some("late".into()),
            true,
            Some(proposed.clone()),
        )
        .await
        .unwrap();
        let expected = RecordedResponse {
            event: "ev-meeting".into(),
            response: Response::Tentative,
            comment: Some("late".into()),
            send_response: true,
            proposed_time: Some(proposed),
        };
        assert_eq!(fake.responses(), std::slice::from_ref(&expected));
        let line = std::fs::read_to_string(&log).unwrap();
        let logged: RecordedResponse = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(logged, expected);

        let unknown = fake
            .respond("nope", Response::Accept, None, true, None)
            .await;
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
            .respond("ev-meeting", Response::Accept, None, true, None)
            .await
            .is_err());
        assert!(fake.responses().is_empty());
    }

    fn written(written: Written) -> Event {
        match written {
            Written::Event(event) => *event,
            Written::Series(series) => panic!("expected an event, got {series:?}"),
        }
    }

    fn input() -> EventInput {
        let at = |h| Utc.with_ymd_and_hms(2026, 11, 4, h, 0, 0).unwrap();
        let mut input = EventInput::new(
            "Planning",
            EventTime::timed(at(9), at(10), "Europe/Berlin").unwrap(),
        );
        input.attendees = vec![AttendeeInput {
            email: "ada@example.com".into(),
            name: None,
            role: AttendeeRole::Required,
        }];
        input.online_meeting = true;
        input
    }

    /// A create adds an event the next sync reports, the user its organizer
    /// with the mailbox's default reminder; a retry with the same transaction
    /// id finds it instead of adding a second; a read-only calendar refuses.
    #[tokio::test]
    async fn a_create_adds_one_event_per_transaction_id() {
        let fake = FakeCalendarBackend::canned();
        let main = fake.list_calendars().await.unwrap().remove(0);
        let baseline = fake.sync_window(&main, november(), None).await.unwrap();

        let event = written(fake.create_event(&main, &input(), "tx-1").await.unwrap());
        assert_eq!(event.calendar, "cal-main");
        assert!(event.is_organizer);
        assert_eq!(event.own_response, OwnResponse::Organizer);
        assert_eq!(event.attendees[0].status, ResponseStatus::NeedsAction);
        assert_eq!(event.reminder, Some(FAKE_DEFAULT_REMINDER));
        assert!(event.online_meeting_url.is_some());
        assert!(event.is_online_meeting);
        assert_eq!(
            event.online_meeting_provider,
            Some(OnlineMeetingProvider::TeamsForBusiness)
        );
        assert_eq!(event.transaction_id.as_deref(), Some("tx-1"));

        let again = written(fake.create_event(&main, &input(), "tx-1").await.unwrap());
        assert_eq!(again, event);
        assert_eq!(fake.events().len(), 4);

        let delta = fake
            .sync_window(&main, november(), baseline.token)
            .await
            .unwrap();
        assert_eq!(delta.upserts, std::slice::from_ref(&event));

        let mut read_only = main.clone();
        read_only.can_edit = false;
        assert!(fake
            .create_event(&read_only, &input(), "tx-2")
            .await
            .is_err());
        assert_eq!(fake.writes().len(), 2);
    }

    /// A create replayed with its transaction id after its fields changed
    /// lands them on the event the first one made, as the Graph backend does
    /// with Graph's `ErrorDuplicateTransactionId`, and the next sync reports
    /// the change; it makes no second event.
    #[tokio::test]
    async fn a_changed_replay_of_a_transaction_patches_its_event() {
        let fake = FakeCalendarBackend::canned();
        let main = fake.list_calendars().await.unwrap().remove(0);
        let first = written(fake.create_event(&main, &input(), "tx-1").await.unwrap());
        let baseline = fake.sync_window(&main, november(), None).await.unwrap();

        let mut changed = input();
        changed.summary = "Planning, moved".into();
        changed.location = Some("Room 2".into());
        let again = written(fake.create_event(&main, &changed, "tx-1").await.unwrap());
        assert_eq!(again.id, first.id);
        assert_eq!(again.summary, "Planning, moved");
        assert_eq!(again.location.as_deref(), Some("Room 2"));
        assert_eq!(again.transaction_id.as_deref(), Some("tx-1"));
        assert_eq!(fake.events().len(), 4);

        let delta = fake
            .sync_window(&main, november(), baseline.token)
            .await
            .unwrap();
        assert_eq!(delta.upserts, std::slice::from_ref(&again));
        let same = written(fake.create_event(&main, &changed, "tx-1").await.unwrap());
        assert_eq!(same, again);
    }

    /// Turning Teams on provisions a link, and turning it off again leaves
    /// it, as Graph does.
    #[tokio::test]
    async fn a_teams_link_once_on_stays_on() {
        let fake = FakeCalendarBackend::canned();
        let patch = |on| EventPatch {
            online_meeting: Some(on),
            ..Default::default()
        };
        let on = written(
            fake.update_event("cal-main", "ev-meeting", &patch(true))
                .await
                .unwrap(),
        );
        let link = on.online_meeting_url.clone();
        assert!(link.is_some());
        let off = written(
            fake.update_event("cal-main", "ev-meeting", &patch(false))
                .await
                .unwrap(),
        );
        assert_eq!(off.online_meeting_url, link);
        assert!(on.is_online_meeting && off.is_online_meeting);
    }

    /// An update changes only what the patch names, a re-invited attendee
    /// keeps their answer, and an unknown event fails.
    #[tokio::test]
    async fn an_update_patches_the_event() {
        let fake = FakeCalendarBackend::canned();
        let patch = EventPatch {
            summary: Some("Project review, moved".into()),
            location: Some(None),
            reminder: Some(None),
            attendees: Some(vec![AttendeeInput {
                email: "Sylvain@example.com".into(),
                name: None,
                role: AttendeeRole::Optional,
            }]),
            ..Default::default()
        };
        let event = written(
            fake.update_event("cal-main", "ev-meeting", &patch)
                .await
                .unwrap(),
        );
        assert_eq!(event.summary, "Project review, moved");
        assert_eq!(event.location, None);
        assert_eq!(event.reminder, None);
        assert_eq!(event.attendees[0].role, AttendeeRole::Optional);
        assert_eq!(event.attendees[0].status, ResponseStatus::NeedsAction);
        assert_eq!(event.organizer.as_ref().unwrap().email, "ada@example.com");
        assert_eq!(
            fake.events().iter().find(|e| e.id == "ev-meeting"),
            Some(&event)
        );

        assert!(fake.update_event("cal-main", "nope", &patch).await.is_err());
        assert_eq!(fake.writes().len(), 1);
    }

    /// A cancel by the organizer and a delete remove the event; a cancel by
    /// an attendee is refused; an event already gone counts as done; every
    /// write lands in the write log.
    #[tokio::test]
    async fn cancel_delete_and_reminders_are_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("writes.jsonl");
        let fake = FakeCalendarBackend::canned().with_write_log(log.clone());
        assert!(fake.cancel_event("ev-meeting", None).await.is_err());
        fake.cancel_event("ev-weekly-2", Some("Off this week".into()))
            .await
            .unwrap();
        fake.cancel_event("ev-weekly-2", None).await.unwrap();
        fake.delete_event("ev-holiday").await.unwrap();
        fake.delete_event("ev-holiday").await.unwrap();
        let until = Utc.with_ymd_and_hms(2026, 11, 3, 8, 50, 0).unwrap();
        fake.dismiss_reminder("ev-meeting").await.unwrap();
        fake.snooze_reminder("ev-meeting", until).await.unwrap();

        let ids: Vec<String> = fake.events().into_iter().map(|e| e.id).collect();
        assert_eq!(ids, ["ev-meeting"]);
        let expected = vec![
            RecordedWrite::Cancel {
                event: "ev-weekly-2".into(),
                comment: Some("Off this week".into()),
            },
            RecordedWrite::Cancel {
                event: "ev-weekly-2".into(),
                comment: None,
            },
            RecordedWrite::Delete {
                event: "ev-holiday".into(),
            },
            RecordedWrite::Delete {
                event: "ev-holiday".into(),
            },
            RecordedWrite::DismissReminder {
                event: "ev-meeting".into(),
            },
            RecordedWrite::SnoozeReminder {
                event: "ev-meeting".into(),
                until,
            },
        ];
        assert_eq!(fake.writes(), expected);
        let logged: Vec<RecordedWrite> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(logged, expected);
        assert!(fake.responses().is_empty());
    }

    /// A weekly series on Monday and Wednesday from Wednesday 4 November to
    /// the 16th, with one attendee.
    fn series_input() -> EventInput {
        let mut input = input();
        input.online_meeting = false;
        input.summary = "Standup".into();
        input.recurrence = Some(Recurrence {
            frequency: Frequency::Weekly,
            interval: 1,
            weekdays: vec![Weekday::Monday, Weekday::Wednesday],
            day_of_month: None,
            week_index: None,
            month: None,
            first_day_of_week: Some(Weekday::Monday),
            start: Some(NaiveDate::from_ymd_opt(2026, 11, 4).unwrap()),
            until: Some(NaiveDate::from_ymd_opt(2026, 11, 16).unwrap()),
            count: None,
        });
        input
    }

    fn occurrence_ids(fake: &FakeCalendarBackend, master: &str) -> Vec<String> {
        let mut ids: Vec<String> = fake
            .events()
            .into_iter()
            .filter(|e| e.series_master_id.as_deref() == Some(master))
            .map(|e| e.id)
            .collect();
        ids.sort();
        ids
    }

    /// A create with a rule makes a series, its master with the series and
    /// its occurrences in the calendar, which a sync reports with the
    /// series; a replay of its transaction finds it.
    #[tokio::test]
    async fn a_recurring_create_makes_a_series_and_its_occurrences() {
        let fake = FakeCalendarBackend::canned();
        let main = fake.list_calendars().await.unwrap().remove(0);
        let baseline = fake.sync_window(&main, november(), None).await.unwrap();
        let Written::Series(series) = fake
            .create_event(&main, &series_input(), "tx-s")
            .await
            .unwrap()
        else {
            panic!("expected a series");
        };
        let master = series.master.as_deref().unwrap();
        assert_eq!(master.kind, EventKind::SeriesMaster);
        assert_eq!(
            master.recurrence.as_deref(),
            Some("Weekly on Monday, Wednesday until 2026-11-16")
        );
        let id = series.master_id.clone();
        assert_eq!(
            occurrence_ids(&fake, &id),
            [
                format!("{id}-20261104"),
                format!("{id}-20261109"),
                format!("{id}-20261111"),
                format!("{id}-20261116"),
            ]
        );
        let delta = fake
            .sync_window(&main, november(), baseline.token)
            .await
            .unwrap();
        assert_eq!(delta.series.len(), 1);
        assert_eq!(delta.upserts.len(), 4);
        assert!(delta
            .upserts
            .iter()
            .all(|e| e.kind == EventKind::Occurrence));
        let Written::Series(again) = fake
            .create_event(&main, &series_input(), "tx-s")
            .await
            .unwrap()
        else {
            panic!("expected the series again");
        };
        assert_eq!(again.master_id, id);
        assert_eq!(fake.series().len(), 1);
    }

    /// A rename of the series reaches its occurrences but the one changed
    /// apart, a new rule expands it afresh and resets that one, and the
    /// sync after either reports the series again without naming the
    /// occurrence the rule dropped; a cancelled occurrence leaves without a
    /// word, and a cancelled series is its master removed.
    #[tokio::test]
    async fn series_writes_behave_as_graph() {
        let fake = FakeCalendarBackend::canned();
        let main = fake.list_calendars().await.unwrap().remove(0);
        let Written::Series(series) = fake
            .create_event(&main, &series_input(), "tx-s")
            .await
            .unwrap()
        else {
            panic!("expected a series");
        };
        let id = series.master_id.clone();
        let second = format!("{id}-20261109");
        let moved = written(
            fake.update_event(
                "cal-main",
                &second,
                &EventPatch {
                    summary: Some("Standup, moved".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        );
        assert_eq!(moved.kind, EventKind::Exception);
        let renamed = EventPatch {
            summary: Some("Daily standup".into()),
            ..Default::default()
        };
        let Written::Series(_) = fake.update_event("cal-main", &id, &renamed).await.unwrap() else {
            panic!("expected the series");
        };
        let summary = |event_id: &str| {
            fake.events()
                .into_iter()
                .find(|e| e.id == event_id)
                .map(|e| e.summary)
        };
        assert_eq!(summary(&second).as_deref(), Some("Standup, moved"));
        assert_eq!(
            summary(&format!("{id}-20261111")).as_deref(),
            Some("Daily standup")
        );

        let token = fake
            .sync_window(&main, november(), None)
            .await
            .unwrap()
            .token;
        let mut rule = series_input().recurrence.unwrap();
        rule.weekdays = vec![Weekday::Monday];
        let reruled = EventPatch {
            recurrence: Some(rule),
            ..Default::default()
        };
        fake.update_event("cal-main", &id, &reruled).await.unwrap();
        assert_eq!(
            occurrence_ids(&fake, &id),
            [second.clone(), format!("{id}-20261116")]
        );
        assert_eq!(summary(&second).as_deref(), Some("Daily standup"));
        let delta = fake.sync_window(&main, november(), token).await.unwrap();
        assert_eq!(delta.upserts.len(), 2);
        assert!(delta.removed.is_empty(), "{:?}", delta.removed);

        fake.cancel_event(&second, None).await.unwrap();
        assert_eq!(occurrence_ids(&fake, &id), [format!("{id}-20261116")]);
        let token = fake
            .sync_window(&main, november(), None)
            .await
            .unwrap()
            .token;
        fake.delete_event(&id).await.unwrap();
        assert!(occurrence_ids(&fake, &id).is_empty());
        assert!(fake.series().is_empty());
        let delta = fake.sync_window(&main, november(), token).await.unwrap();
        assert_eq!(delta.removed, [id]);
    }

    #[tokio::test]
    async fn a_fake_without_the_scope_refuses_every_write() {
        let fake = FakeCalendarBackend::canned().missing_scope();
        let main = FakeCalendarBackend::canned().calendars.remove(0);
        assert!(fake.create_event(&main, &input(), "tx").await.is_err());
        assert!(fake
            .update_event("cal-main", "ev-meeting", &EventPatch::default())
            .await
            .is_err());
        assert!(fake.cancel_event("ev-weekly-2", None).await.is_err());
        assert!(fake.delete_event("ev-holiday").await.is_err());
        assert!(fake.dismiss_reminder("ev-meeting").await.is_err());
        assert!(fake.writes().is_empty());
        assert_eq!(fake.events().len(), 3);
    }

    #[test]
    fn the_env_spec_parses_its_calendars_and_events() {
        let canned = FakeCalendarBackend::canned();
        let spec = serde_json::json!({
            "calendars": canned.calendars,
            "events": canned.events(),
            "missing_scope": true,
            "features": ["write", "online-meeting"],
        });
        let parsed: FakeSpec = serde_json::from_value(spec).unwrap();
        assert_eq!(parsed.calendars, canned.calendars);
        assert_eq!(parsed.events, canned.events());
        assert!(parsed.missing_scope && parsed.log.is_none());
        assert_eq!(
            parsed.features,
            [CalendarFeature::Write, CalendarFeature::OnlineMeeting]
        );
    }

    /// An address the fake knows answers its blocks with their view, any
    /// other has no free/busy, and a fake without the scope refuses.
    #[tokio::test]
    async fn free_busy_answers_the_known_addresses_and_none_for_the_others() {
        let at = |h: u32| Utc.with_ymd_and_hms(2026, 11, 3, h, 0, 0).unwrap();
        let busy = ScheduleItem {
            start: at(9),
            end: at(10),
            status: ShowAs::Busy,
            subject: Some("Project review".into()),
            location: None,
        };
        let fake = FakeCalendarBackend::canned().with_free_busy("Ada@example.com", vec![busy]);
        let query = FreeBusyQuery {
            addresses: vec!["ada@example.com".into(), "bob@gmail.com".into()],
            window: CalendarWindow {
                start: at(8),
                end: at(11),
            },
            interval: 60,
        };
        let schedules = fake.free_busy(&query).await.unwrap();
        assert_eq!(schedules[0].view, "020");
        assert_eq!(schedules[0].items.len(), 1);
        assert_eq!(schedules[1].error.as_deref(), Some(FAKE_NO_FREE_BUSY));
        assert!(fake.missing_scope().free_busy(&query).await.is_err());
    }
}
