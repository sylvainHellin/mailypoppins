//! The calendar reminders (PERSO-255, `docs/plans/calendar.md`, phase 2).
//!
//! The daemon computes each event's reminder from the calendar cache, which
//! holds the reminder setting of every occurrence (an occurrence the delta
//! trims takes it from its series master). One task keeps the next due
//! minute (start minus the setting) of the events of the coming
//! [`LOOKAHEAD`] and sleeps until it; every change to the cache, which
//! publishes `calendar.changed`, wakes it to compute again, so a reminder
//! fires at its due minute.
//!
//! A cancelled event, a declined one, one without a reminder and one in a
//! hidden calendar never fire. A reminder that fell due while the daemon was
//! down fires once at start-up if its event has not ended, as Outlook shows an
//! overdue reminder.
//!
//! What fired, was dismissed or is snoozed is recorded in
//! [`crate::store::reminders`], which survives a `calendar.rebuild`. A firing
//! is published as `calendar.reminder` to every client, naming the one
//! connection that raises the system notification (`presenter_connection`)
//! and its kind (`presenter`): the most recently bootstrapped desktop app,
//! else the most recently bootstrapped TUI ([`CanonicalState::presenter`]).
//! A fired reminder that stops qualifying (its event cancelled, declined,
//! hidden or its reminder turned off, which is what a dismissal in Outlook
//! does) leaves the pending list, one that qualifies again (its calendar
//! shown again) comes back to it, and `calendar.reminders_changed` says so
//! either way ([`list_changed`]).
//!
//! The clock is the system's, except under [`FAKE_CLOCK_SPEED_ENV`], which a
//! daemon slice test sets to run the reminder clock faster than real time
//! from the daemon's start.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use anyhow::Result;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use log::{info, warn};
use tokio::sync::{watch, Notify};

use mp_protocol::events::{KIND_CALENDAR_REMINDER, KIND_CALENDAR_REMINDERS_CHANGED};
use mp_protocol::reminder::{CalendarReminder, CalendarRemindersChanged};

use crate::calendar_model::{CalendarWindow, Event, OwnResponse, Tz};
use crate::config::AccountConfig;
use crate::store::reminders::{self as record, ReminderRow, ReminderState};
use crate::store::Store;

use super::config::ConfigStore;
use super::state::events::Event as DaemonEvent;
use super::state::{CanonicalState, ConnectionId};

/// `MAILYPOPPINS_DAEMON_FAKE_CLOCK_SPEED=<n>`: the reminder clock starts at
/// the daemon's start and runs `n` times faster than real time, so a test
/// sees a snooze of a minute end within a second.
pub const FAKE_CLOCK_SPEED_ENV: &str = "MAILYPOPPINS_DAEMON_FAKE_CLOCK_SPEED";

/// How far ahead the task looks for a reminder: Outlook's longest setting is
/// two weeks before the start.
pub const LOOKAHEAD: Duration = Duration::days(15);

/// The longest the task sleeps without computing again, in real time: a
/// laptop asleep past a due minute stops the monotonic clock a sleep counts
/// on, and the next computation catches up within this.
const MAX_SLEEP: std::time::Duration = std::time::Duration::from_secs(60);

/// The longest snooze `calendar.snooze_reminder` takes, two weeks in minutes.
pub const MAX_SNOOZE_MINUTES: u32 = 20_160;

/// The reminder clock: the system's, or a faster one from the daemon's start
/// ([`FAKE_CLOCK_SPEED_ENV`]).
#[derive(Debug, Clone, Copy)]
pub struct ReminderClock {
    origin: DateTime<Utc>,
    started: Instant,
    speed: u32,
}

impl ReminderClock {
    /// The system clock.
    pub fn real() -> ReminderClock {
        ReminderClock::with_speed(1)
    }

    /// A clock starting now and running `speed` times faster.
    pub fn with_speed(speed: u32) -> ReminderClock {
        ReminderClock {
            origin: Utc::now(),
            started: Instant::now(),
            speed: speed.max(1),
        }
    }

    /// The system clock, or the one [`FAKE_CLOCK_SPEED_ENV`] asks for.
    pub fn from_env() -> ReminderClock {
        match std::env::var(FAKE_CLOCK_SPEED_ENV)
            .ok()
            .and_then(|raw| raw.trim().parse::<u32>().ok())
        {
            Some(speed) if speed > 1 => ReminderClock::with_speed(speed),
            _ => ReminderClock::real(),
        }
    }

    pub fn now(&self) -> DateTime<Utc> {
        if self.speed == 1 {
            return Utc::now();
        }
        let elapsed = self.started.elapsed() * self.speed;
        self.origin + Duration::from_std(elapsed).unwrap_or_default()
    }

    /// The real time until the clock reads `at`, at most [`MAX_SLEEP`].
    fn real_wait(&self, at: Option<DateTime<Utc>>) -> std::time::Duration {
        let Some(at) = at else {
            return MAX_SLEEP;
        };
        let ahead = (at - self.now()).to_std().unwrap_or_default();
        (ahead / self.speed).clamp(std::time::Duration::from_millis(50), MAX_SLEEP)
    }
}

/// When an event's reminder falls due, and the span it reminds of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Due {
    pub fire_at: DateTime<Utc>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub minutes: u32,
}

/// The reminder of `event` seen from `viewer` (which places an all-day
/// event's midnights), or `None` for an event that does not remind: no
/// reminder setting, cancelled, or declined.
pub fn due(event: &Event, viewer: Tz) -> Option<Due> {
    let minutes = event.reminder?;
    if event.cancelled || event.own_response == OwnResponse::Declined {
        return None;
    }
    let (start, end) = event.utc_interval(viewer);
    let (start, end) = (whole_second(start), whole_second(end));
    Some(Due {
        fire_at: start - Duration::minutes(i64::from(minutes)),
        start,
        end: end.max(start),
        minutes,
    })
}

/// `at` without its fraction of a second: the record keeps whole seconds,
/// and a due minute is compared with the one it recorded.
fn whole_second(at: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp(at.timestamp(), 0).unwrap_or(at)
}

/// The reminder of `event` on the wire, without a presenter.
pub fn to_wire(event: &Event, due: &Due) -> CalendarReminder {
    CalendarReminder {
        account: event.account.clone(),
        event: event.id.clone(),
        summary: event.summary.clone(),
        start: due.start.to_rfc3339_opts(SecondsFormat::Secs, true),
        all_day: event.is_all_day(),
        location: event.location.clone(),
        online_meeting_url: event.online_meeting_url.clone(),
        minutes_before: due.minutes,
        presenter: None,
        presenter_connection: None,
    }
}

/// Name `presenter`, the connection elected by [`CanonicalState::presenter`],
/// on `reminder`: its kind and its connection id, or neither.
pub fn present(reminder: &mut CalendarReminder, presenter: Option<(ConnectionId, &str)>) {
    reminder.presenter = presenter.map(|(_, kind)| kind.to_string());
    reminder.presenter_connection = presenter.map(|(conn, _)| conn.0);
}

/// The reminding events of `account` that have not ended by `now`, with the
/// rows recorded for them.
struct Scan {
    events: Vec<(Event, Due)>,
    rows: HashMap<String, ReminderRow>,
}

fn scan(store: &Store, account: &str, now: DateTime<Utc>, viewer: Tz) -> Result<Scan> {
    let window = CalendarWindow {
        start: now,
        end: now + LOOKAHEAD,
    };
    let events = crate::store::calendar::events_in_range(store, &[account], window, viewer, true)?
        .into_iter()
        .filter_map(|event| {
            let due = due(&event, viewer)?;
            (due.end > now).then_some((event, due))
        })
        .collect();
    let rows = record::load(store, account)?
        .into_iter()
        .map(|row| (row.event.clone(), row))
        .collect();
    Ok(Scan { events, rows })
}

/// The row of an event recorded for its current due minute: a row recorded
/// for another minute (the event moved, or its setting changed) is stale.
fn current<'a>(scan: &'a Scan, event: &Event, due: &Due) -> Option<&'a ReminderRow> {
    scan.rows
        .get(&event.id)
        .filter(|row| row.fire_at == due.fire_at)
}

/// What one computation over an account found.
#[derive(Debug, Default)]
pub struct PassOutcome {
    /// The reminders that fell due now, without a presenter.
    pub fired: Vec<CalendarReminder>,
    /// Every fired reminder still waiting for the user, by event id.
    pub pending: BTreeSet<String>,
    /// The next minute something falls due, a reminder or a snooze.
    pub next: Option<DateTime<Utc>>,
}

/// Fire what fell due on `account` by `now`, record it, prune the rows of
/// ended events, and answer what is pending and when to look again.
pub fn pass(store: &Store, account: &str, now: DateTime<Utc>, viewer: Tz) -> Result<PassOutcome> {
    record::prune(store, account, now)?;
    let scan = scan(store, account, now, viewer)?;
    let mut outcome = PassOutcome::default();
    let later = |at: DateTime<Utc>, next: &mut Option<DateTime<Utc>>| {
        *next = Some(next.map_or(at, |next| next.min(at)));
    };
    for (event, due) in &scan.events {
        let fire = match current(&scan, event, due).map(|row| row.state) {
            None if due.fire_at <= now => true,
            None => {
                later(due.fire_at, &mut outcome.next);
                false
            }
            Some(ReminderState::Fired) => {
                outcome.pending.insert(event.id.clone());
                false
            }
            Some(ReminderState::Dismissed) => false,
            Some(ReminderState::Snoozed(until)) if until <= now => true,
            Some(ReminderState::Snoozed(until)) => {
                later(until, &mut outcome.next);
                false
            }
        };
        if fire {
            record::put(
                store,
                &ReminderRow {
                    account: account.to_string(),
                    event: event.id.clone(),
                    fire_at: due.fire_at,
                    end_at: due.end,
                    state: ReminderState::Fired,
                },
            )?;
            outcome.pending.insert(event.id.clone());
            outcome.fired.push(to_wire(event, due));
        }
    }
    Ok(outcome)
}

/// The fired reminders of `account` waiting for the user at `now`, the
/// earliest start first, without a presenter: `calendar.reminders`.
pub fn pending(
    store: &Store,
    account: &str,
    now: DateTime<Utc>,
    viewer: Tz,
) -> Result<Vec<CalendarReminder>> {
    let scan = scan(store, account, now, viewer)?;
    let mut waiting: Vec<(DateTime<Utc>, CalendarReminder)> = scan
        .events
        .iter()
        .filter(|(event, due)| {
            current(&scan, event, due).is_some_and(|row| row.state == ReminderState::Fired)
        })
        .map(|(event, due)| (due.start, to_wire(event, due)))
        .collect();
    waiting.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.event.cmp(&b.1.event)));
    Ok(waiting.into_iter().map(|(_, reminder)| reminder).collect())
}

/// What `calendar.dismiss_reminder` or `calendar.snooze_reminder` records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Dismiss,
    Snooze(DateTime<Utc>),
}

/// Why a reminder cannot be dismissed or snoozed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The cache holds no such event.
    NoEvent,
    /// The event does not remind: no setting, cancelled or declined.
    NoReminder,
    /// A snooze of a reminder already dismissed, which the server ignores
    /// too.
    Dismissed,
}

/// Record `decision` on `event` of `account`, whether its reminder fired
/// yet or not, so a dismissal ahead of time keeps it from firing.
pub fn decide(
    store: &Store,
    account: &str,
    event: &str,
    decision: Decision,
    viewer: Tz,
) -> Result<std::result::Result<(), Refusal>> {
    let Some(cached) = crate::store::calendar::get_event(store, account, event)? else {
        return Ok(Err(Refusal::NoEvent));
    };
    let Some(due) = due(&cached, viewer) else {
        return Ok(Err(Refusal::NoReminder));
    };
    let recorded = record::get(store, account, event)?.filter(|row| row.fire_at == due.fire_at);
    let state = match decision {
        Decision::Dismiss => ReminderState::Dismissed,
        Decision::Snooze(_)
            if recorded.is_some_and(|row| row.state == ReminderState::Dismissed) =>
        {
            return Ok(Err(Refusal::Dismissed));
        }
        Decision::Snooze(until) => ReminderState::Snoozed(until),
    };
    record::put(
        store,
        &ReminderRow {
            account: account.to_string(),
            event: event.to_string(),
            fire_at: due.fire_at,
            end_at: due.end,
            state,
        },
    )?;
    Ok(Ok(()))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The reminder task's shared half: its clock, its wake-up, and what it last
/// saw pending per account.
#[derive(Debug)]
pub struct Reminders {
    clock: ReminderClock,
    wake: Notify,
    pending: Mutex<HashMap<String, BTreeSet<String>>>,
}

impl Reminders {
    pub fn new(clock: ReminderClock) -> Reminders {
        Reminders {
            clock,
            wake: Notify::new(),
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// The reminder clock's now.
    pub fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    /// Compute again now: the cache or a decision moved.
    pub fn poke(&self) {
        self.wake.notify_one();
    }

    /// Forget that `event` of `account` was pending, after a decision that
    /// already published `calendar.reminders_changed` for it.
    pub fn forget(&self, account: &str, event: &str) {
        if let Some(pending) = lock(&self.pending).get_mut(account) {
            pending.remove(event);
        }
    }
}

/// The `calendar.reminders_changed` of `account`.
pub fn reminders_changed(account: &str) -> DaemonEvent {
    DaemonEvent::Lifecycle {
        kind: KIND_CALENDAR_REMINDERS_CHANGED,
        payload: serde_json::to_value(CalendarRemindersChanged {
            account: account.to_string(),
        })
        .unwrap_or_else(|_| serde_json::json!({"account": account})),
    }
}

/// The `calendar.reminder` of one firing.
fn reminder_event(reminder: &CalendarReminder) -> Option<DaemonEvent> {
    match serde_json::to_value(reminder) {
        Ok(payload) => Some(DaemonEvent::Lifecycle {
            kind: KIND_CALENDAR_REMINDER,
            payload,
        }),
        Err(e) => {
            warn!("[reminders] could not serialise a reminder: {e}");
            None
        }
    }
}

/// Whether the pending list of an account moved other than by the reminders
/// that just `fired`, each of which its own `calendar.reminder` announces:
/// one left it (dismissed in Outlook, cancelled, declined, hidden) or one
/// came back without firing (its calendar shown again). The first pass after
/// the start has nothing to compare with and says nothing, since a client
/// reads the list at its bootstrap.
fn list_changed(
    before: Option<&BTreeSet<String>>,
    fired: &[String],
    pending: &BTreeSet<String>,
) -> bool {
    let Some(before) = before else {
        return false;
    };
    let mut expected = before.clone();
    expected.extend(fired.iter().cloned());
    expected != *pending
}

/// The accounts the task computes for: a calendar backend and a store.
pub fn reminding_accounts(accounts: &[AccountConfig]) -> Vec<AccountConfig> {
    accounts
        .iter()
        .filter(|account| crate::calendar_backend::has_backend(account))
        .filter(|account| super::methods::account::ready_account(accounts, &account.name).is_ok())
        .cloned()
        .collect()
}

/// Run the reminder task until the daemon stops its watchers.
pub fn spawn(
    reminders: Arc<Reminders>,
    canonical: Arc<CanonicalState>,
    config: Arc<ConfigStore>,
    mut stopping: watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        loop {
            let next = run_once(&reminders, &canonical, &config).await;
            let wait = reminders.clock.real_wait(next);
            tokio::select! {
                changed = stopping.changed() => {
                    if changed.is_err() || *stopping.borrow() {
                        return;
                    }
                }
                _ = reminders.wake.notified() => {}
                _ = tokio::time::sleep(wait) => {}
            }
        }
    });
}

/// One computation over every reminding account, published; answers the
/// next minute something falls due.
async fn run_once(
    reminders: &Reminders,
    canonical: &CanonicalState,
    config: &ConfigStore,
) -> Option<DateTime<Utc>> {
    let accounts = reminding_accounts(&config.accounts());
    let now = reminders.now();
    let outcomes = tokio::task::spawn_blocking(move || {
        let viewer = crate::calendar_view::local_zone();
        accounts
            .into_iter()
            .filter_map(|account| {
                let name = account.name;
                let outcome = Store::open(crate::config::store_path(&name))
                    .and_then(|store| pass(&store, &name, now, viewer));
                match outcome {
                    Ok(outcome) => Some((name, outcome)),
                    Err(e) => {
                        warn!("[reminders] {name}: {e:#}");
                        None
                    }
                }
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_else(|e| {
        warn!("[reminders] the computation panicked: {e}");
        Vec::new()
    });
    let mut next: Option<DateTime<Utc>> = None;
    for (account, outcome) in outcomes {
        let presenter = canonical.presenter();
        let fired: Vec<String> = outcome.fired.iter().map(|r| r.event.clone()).collect();
        for mut reminder in outcome.fired {
            info!(
                "[reminders] {account}: {} is due (presenter {})",
                reminder.event,
                presenter.map_or("none".to_string(), |(conn, kind)| format!(
                    "{kind} connection {}",
                    conn.0
                ))
            );
            present(&mut reminder, presenter);
            if let Some(event) = reminder_event(&reminder) {
                canonical.publish(event);
            }
        }
        let changed = {
            let mut seen = lock(&reminders.pending);
            let before = seen.insert(account.clone(), outcome.pending.clone());
            list_changed(before.as_ref(), &fired, &outcome.pending)
        };
        if changed {
            canonical.publish(reminders_changed(&account));
        }
        if let Some(at) = outcome.next {
            next = Some(next.map_or(at, |next| next.min(at)));
        }
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar_backend::SyncResult;
    use crate::calendar_model::{Calendar, EventKind, EventTime, Sensitivity, ShowAs};
    use chrono::TimeZone;

    fn utc(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 4, h, m, 0).unwrap()
    }

    fn berlin() -> Tz {
        "Europe/Berlin".parse().unwrap()
    }

    fn event(id: &str, start: DateTime<Utc>, reminder: Option<u32>) -> Event {
        Event {
            account: "work".into(),
            calendar: "cal".into(),
            id: id.into(),
            uid: format!("{id}@example.com"),
            recurrence_id: None,
            series_master_id: None,
            kind: EventKind::Single,
            summary: format!("Meeting {id}"),
            time: EventTime::timed(start, start + Duration::hours(1), "Europe/Berlin").unwrap(),
            location: None,
            body: None,
            organizer: None,
            attendees: Vec::new(),
            own_response: OwnResponse::Accepted,
            recurrence: None,
            reminder,
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
        }
    }

    fn store_with(events: Vec<Event>) -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        crate::store::calendar::upsert_calendars(
            &store,
            "work",
            &[Calendar {
                account: "work".into(),
                id: "cal".into(),
                name: "Calendar".into(),
                color: None,
                is_default: true,
                can_edit: true,
                visible: true,
                online_meeting_providers: Vec::new(),
            }],
        )
        .unwrap();
        set_events(&store, events);
        (dir, store)
    }

    fn set_events(store: &Store, events: Vec<Event>) {
        crate::store::calendar::apply_sync(
            store,
            "work",
            "cal",
            &SyncResult {
                upserts: events,
                full_resync: true,
                ..SyncResult::default()
            },
            crate::calendar_model::window_for(utc(9, 0)),
            utc(9, 0),
        )
        .unwrap();
    }

    fn fired(outcome: &PassOutcome) -> Vec<&str> {
        outcome.fired.iter().map(|r| r.event.as_str()).collect()
    }

    /// A reminder fires at its due minute, once, and stays pending; one
    /// overdue at start-up fires if its event has not ended; the next due
    /// minute is what the task sleeps until.
    #[test]
    fn a_reminder_fires_once_at_its_due_minute() {
        let (_dir, store) = store_with(vec![
            event("soon", utc(10, 0), Some(15)),
            event("overdue", utc(8, 50), Some(15)),
            event("ended", utc(7, 0), Some(15)),
            event("none", utc(10, 0), None),
        ]);
        let first = pass(&store, "work", utc(9, 0), berlin()).unwrap();
        assert_eq!(fired(&first), ["overdue"]);
        assert_eq!(first.next, Some(utc(9, 45)));
        assert_eq!(first.fired[0].minutes_before, 15);
        assert_eq!(first.fired[0].start, "2026-11-04T08:50:00Z");

        let early = pass(&store, "work", utc(9, 44), berlin()).unwrap();
        assert!(early.fired.is_empty());
        let due = pass(&store, "work", utc(9, 45), berlin()).unwrap();
        assert_eq!(fired(&due), ["soon"]);
        let again = pass(&store, "work", utc(9, 46), berlin()).unwrap();
        assert!(again.fired.is_empty());
        assert_eq!(
            again.pending.iter().map(String::as_str).collect::<Vec<_>>(),
            ["overdue", "soon"]
        );
        let listed = pending(&store, "work", utc(9, 46), berlin()).unwrap();
        assert_eq!(
            listed.iter().map(|r| r.event.as_str()).collect::<Vec<_>>(),
            ["overdue", "soon"]
        );
    }

    /// A start with a fraction of a second, as a clock-derived time has,
    /// matches the whole seconds the record keeps, so it fires once.
    #[test]
    fn a_start_with_a_fraction_of_a_second_fires_once() {
        let start = utc(10, 0) + Duration::milliseconds(750);
        let (_dir, store) = store_with(vec![event("ev", start, Some(15))]);
        assert_eq!(
            fired(&pass(&store, "work", utc(9, 50), berlin()).unwrap()),
            ["ev"]
        );
        assert!(pass(&store, "work", utc(9, 51), berlin())
            .unwrap()
            .fired
            .is_empty());
        assert_eq!(
            pending(&store, "work", utc(9, 51), berlin()).unwrap().len(),
            1
        );
    }

    /// A cancelled or declined event never reminds, and a fired one that is
    /// cancelled later leaves the pending list.
    #[test]
    fn cancelled_and_declined_events_never_fire() {
        let mut cancelled = event("cancelled", utc(9, 30), Some(60));
        cancelled.cancelled = true;
        let mut declined = event("declined", utc(9, 30), Some(60));
        declined.own_response = OwnResponse::Declined;
        let live = event("live", utc(9, 30), Some(60));
        let (_dir, store) = store_with(vec![cancelled, declined, live.clone()]);
        let outcome = pass(&store, "work", utc(9, 0), berlin()).unwrap();
        assert_eq!(fired(&outcome), ["live"]);

        let mut gone = live;
        gone.cancelled = true;
        set_events(&store, vec![gone]);
        let outcome = pass(&store, "work", utc(9, 1), berlin()).unwrap();
        assert!(outcome.pending.is_empty());
        assert!(pending(&store, "work", utc(9, 1), berlin())
            .unwrap()
            .is_empty());
    }

    /// A snooze fires again once it ends; a dismissal never does, until the
    /// event moves to another minute; a snooze of a dismissed reminder is
    /// refused.
    #[test]
    fn snooze_and_dismiss_decide_the_next_firing() {
        let (_dir, store) = store_with(vec![event("ev", utc(10, 0), Some(15))]);
        assert_eq!(
            fired(&pass(&store, "work", utc(9, 45), berlin()).unwrap()),
            ["ev"]
        );
        decide(&store, "work", "ev", Decision::Snooze(utc(9, 50)), berlin())
            .unwrap()
            .unwrap();
        let snoozed = pass(&store, "work", utc(9, 46), berlin()).unwrap();
        assert!(snoozed.fired.is_empty());
        assert!(snoozed.pending.is_empty());
        assert_eq!(snoozed.next, Some(utc(9, 50)));
        assert_eq!(
            fired(&pass(&store, "work", utc(9, 50), berlin()).unwrap()),
            ["ev"]
        );

        decide(&store, "work", "ev", Decision::Dismiss, berlin())
            .unwrap()
            .unwrap();
        let dismissed = pass(&store, "work", utc(9, 51), berlin()).unwrap();
        assert!(dismissed.fired.is_empty() && dismissed.pending.is_empty());
        assert_eq!(
            decide(&store, "work", "ev", Decision::Snooze(utc(9, 55)), berlin()).unwrap(),
            Err(Refusal::Dismissed)
        );

        set_events(&store, vec![event("ev", utc(10, 30), Some(15))]);
        let moved = pass(&store, "work", utc(9, 52), berlin()).unwrap();
        assert_eq!(moved.next, Some(utc(10, 15)));
        assert_eq!(
            fired(&pass(&store, "work", utc(10, 15), berlin()).unwrap()),
            ["ev"]
        );
    }

    /// A dismissal ahead of the due minute keeps the reminder from firing,
    /// and an event the cache lacks or that does not remind is refused.
    #[test]
    fn a_decision_ahead_of_time_and_the_refusals() {
        let (_dir, store) = store_with(vec![
            event("ev", utc(10, 0), Some(15)),
            event("quiet", utc(10, 0), None),
        ]);
        decide(&store, "work", "ev", Decision::Dismiss, berlin())
            .unwrap()
            .unwrap();
        assert!(pass(&store, "work", utc(9, 45), berlin())
            .unwrap()
            .fired
            .is_empty());
        assert_eq!(
            decide(&store, "work", "quiet", Decision::Dismiss, berlin()).unwrap(),
            Err(Refusal::NoReminder)
        );
        assert_eq!(
            decide(&store, "work", "nope", Decision::Dismiss, berlin()).unwrap(),
            Err(Refusal::NoEvent)
        );
    }

    /// The pending list moving either way is a change, a firing alone is not
    /// (its `calendar.reminder` says it), and the first pass says nothing.
    #[test]
    fn the_pending_list_changes_both_ways() {
        let set = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<BTreeSet<_>>();
        let fired = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        assert!(!list_changed(None, &[], &set(&["a"])));
        assert!(!list_changed(Some(&set(&["a"])), &[], &set(&["a"])));
        assert!(list_changed(Some(&set(&["a"])), &[], &set(&[])), "hidden");
        assert!(list_changed(Some(&set(&[])), &[], &set(&["a"])), "shown");
        assert!(!list_changed(Some(&set(&[])), &fired(&["a"]), &set(&["a"])));
        assert!(list_changed(
            Some(&set(&["b"])),
            &fired(&["a"]),
            &set(&["a"])
        ));
    }

    /// The fake clock runs from its origin at its speed, and the task never
    /// sleeps longer than a minute of real time.
    #[test]
    fn the_clock_scales_its_waits() {
        let clock = ReminderClock::with_speed(60);
        let wait = clock.real_wait(Some(clock.now() + Duration::minutes(5)));
        assert!(wait <= std::time::Duration::from_secs(5), "{wait:?}");
        assert!(wait >= std::time::Duration::from_secs(4), "{wait:?}");
        assert_eq!(ReminderClock::real().real_wait(None), MAX_SLEEP);
        let real = ReminderClock::real();
        assert_eq!(
            real.real_wait(Some(real.now() + Duration::hours(3))),
            MAX_SLEEP
        );
    }
}
